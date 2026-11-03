// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The agentic tool loop behind a chat turn.

use super::*;

mod budget;

use budget::*;

impl ConversationService {
    /// Run the agentic tool loop and stream the result. Each round is a single
    /// streaming pass ([`AiService::chat_stream_turn`]) that yields assistant text
    /// **and** tool calls inline — so prose arrives token-by-token while tool
    /// activity surfaces live, from the same model call (the way the Vercel AI SDK
    /// works). When a round makes tool calls we execute them, feed the results
    /// back, and stream the next round; when a round answers in prose with no tool
    /// calls, that streamed prose is the final answer. A simple chat that needs no
    /// tools is therefore exactly one streaming call.
    ///
    /// The whole loop runs as a server-owned task. SSE and WebSocket clients are
    /// detachable viewers; closing or refreshing one never aborts execution.
    /// Completed turns persist `content` (all prose, for history replay) plus ordered
    /// `parts` (text/tool segments in occurrence order) and the executed `tools`,
    /// so a reload renders identically to the live stream.
    #[cfg(test)]
    pub(super) async fn try_tool_loop(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        let request_metadata = RequestMetadata {
            ip_address: String::new(),
            user_agent: String::new(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: String::new(),
            scheme: String::new(),
            host: String::new(),
            is_secure: false,
        };
        self.try_tool_loop_in_workspace(
            conv,
            base_messages,
            provider,
            tools,
            auth,
            &request_metadata,
            None,
            Vec::new(),
            None,
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn try_tool_loop_in_workspace(
        &self,
        conv: &ai_conversations::Model,
        base_messages: Vec<ChatMessage>,
        provider: Option<Arc<dyn ConversationContextProvider>>,
        tools: Vec<ChatTool>,
        auth: &AuthContext,
        request_metadata: &RequestMetadata,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        harness_project_ids: Vec<i32>,
        harness_workspace: Option<temps_ai::HarnessWorkspace>,
        sandbox_environment: temps_ai::SensitiveEnvironment,
        sandbox_attachments: Vec<temps_ai::SandboxAttachment>,
        active_turn_id: Option<String>,
        should_capture_session_title: bool,
        draft_message_id: Option<i64>,
    ) -> Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>> {
        let max_turn_duration = self.turn_time_limit().await;
        let round_limits = RoundLimits::for_context(&conv.context_type);
        // Directive appended before the final, tool-free answer so the model
        // writes real prose from the evidence instead of narrating another tool
        // call it would like to make.
        const FINAL_DIRECTIVE: &str =
            "You have no more tool calls available. Using ONLY the tool results above, write \
             your final answer to my request now, in plain prose. Do not emit tool-call JSON \
             or describe tools you would call. If the data is insufficient, briefly state what \
             you found and what is still missing.";

        #[allow(
            clippy::disallowed_methods,
            reason = "legacy runtime env config; follow-up: move to a DB-backed setting (P0.7 follow-up). TEMPS_INTERNAL_API_URL, only read when no ConfigService is wired"
        )]
        let harness_internal_api_url = if harness_workspace.is_some() {
            Some(match &self.config {
                Some(config) => config.resolve_internal_url().await,
                None => std::env::var("TEMPS_INTERNAL_API_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "http://host.docker.internal:8080".to_string()),
            })
        } else {
            None
        };

        // Own everything the server task needs (the service borrows `&self`).
        let ai = self.ai.clone();
        let db = self.db.clone();
        let api_tools = self.providers.get("__api_tools__").cloned();
        let repo_tools = self.providers.get("__repo_tools__").cloned();
        let conv_id = conv.id;
        let conv_public_id = conv.public_id.clone();
        let pending_permissions = self.pending_permissions.clone();
        let project_id = conv.project_id;
        let context_type = conv.context_type.clone();
        let context_id = conv.context_id.clone();
        let ai_provider = conv.ai_provider.clone();
        let ai_model = conv.ai_model.clone();
        let ai_thinking_level = conv.ai_thinking_level.clone();
        let ai_permission_mode = conv.ai_permission_mode.clone();
        let resume_session_id = harness_workspace.as_ref().and(conv.cli_session_id.clone());
        let auto_approve_provider_tools = Arc::new(AtomicBool::new(
            permission_mode_auto_approves_provider_tools(&ai_permission_mode),
        ));
        let task_auto_approve_provider_tools = auto_approve_provider_tools.clone();
        let permission_mode = Arc::new(Mutex::new(ai_permission_mode.clone()));
        let task_permission_mode = permission_mode.clone();
        // An autofix incident chat (ADR-054) runs with nobody watching: its
        // platform writes are refused outright instead of waiting for an
        // approval that would never come. Read once per turn from the chat.
        let autofix_incident = crate::autofix::incident_of_conversation(conv.metadata.as_ref());
        let application_id = conv.application_id;
        let initial_conversation_title = conv.title.clone();
        let auth = auth.clone();
        let request_metadata = request_metadata.clone();
        // Write support clones (None when not wired or project toggle is off).
        let write_handle_opt = self
            .write_support
            .as_ref()
            .and_then(|ws| ws.write_handle.get());
        let pending_svc_opt = self.write_support.as_ref().map(|ws| ws.pending.clone());
        let write_support_audit = self.write_support.as_ref().map(|ws| ws.audit.clone());
        let goal_support_opt = (conv.context_type
            == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE)
            .then(|| self.goal_support.clone())
            .flatten();
        let sandbox_link_opt = self
            .sandbox_link_for(conv)
            .map(|link| link.with_project_access(project_access_checker.clone()));
        let blueprint_support_opt = (conv.context_type
            == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE)
            .then(|| self.blueprint_support.clone())
            .flatten()
            .map(|mut support| {
                support.project_access = project_access_checker.clone();
                support
            });
        // Offered exactly where `temps_write` is: a credential has no use
        // outside an operation that can be proposed.
        let secret_support_opt = write_handle_opt
            .is_some()
            .then(|| self.secret_support.clone())
            .flatten();

        // The turn runs independently of the SSE reader (a browser refresh
        // only drops `rx`), so it must never wait on a slow viewer: a bounded
        // channel would stall the turn itself. The queue holds at most one
        // turn's events, which the turn's own limits bound.
        #[allow(
            clippy::disallowed_methods,
            reason = "one turn's events; the turn must not block on a slow SSE viewer"
        )]
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();

        let turn_live = self.broadcast_sender_for(conv_id);
        let monitor_live = turn_live.clone();
        let monitor_db = self.db.clone();
        let monitor_title = (conv.context_type != "application").then(|| {
            (
                self.ai.clone(),
                conv.project_id,
                conv.ai_provider.clone(),
                conv.ai_model.clone(),
            )
        });
        let active_turns = self.active_turns.clone();
        let harness_mcp_entries = self.harness_mcp_entries.clone();
        let monitor_turn_id = active_turn_id.clone();
        let task_turn_id = active_turn_id.clone();
        let monitor_started = tokio::time::Instant::now();
        let startup_event_tx = tx.clone();
        let startup_live = turn_live.clone();
        let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel::<()>();

        // The loop, event publication, and persistence run independently of
        // the returned SSE receiver. Browser refresh only drops `rx`.
        let turn_task = tokio::spawn(async move {
            // `claim_turn` is durable before workspace/provider preparation is
            // complete. Stop can therefore arrive before this task is entered
            // in `active_turns`. Do not execute any harness or provider work
            // until the post-registration database check below proves this
            // exact turn still owns the server-side claim.
            if startup_receiver.await.is_err() {
                return true;
            }
            let mut messages = base_messages;
            let execution_state = Arc::new(tokio::sync::Mutex::new(ToolExecutionState {
                goals: goal_support_opt.clone(),
                secrets: secret_support_opt.clone(),
                sandbox: sandbox_link_opt.clone(),
                blueprints: blueprint_support_opt.clone(),
                incident: autofix_incident.clone().map(|public_id| IncidentToolGuard {
                    public_id,
                    db: db.clone(),
                }),
                ..Default::default()
            }));
            let interaction_conv_public_id = conv_public_id.clone();
            let interaction_registry = pending_permissions.clone();
            type PermissionRegistrar = Arc<
                dyn Fn(
                        temps_ai::streaming::PermissionRequest,
                        PendingPermissionOrigin,
                    )
                        -> BoxFuture<'static, Result<PermissionResolution, temps_ai::AiError>>
                    + Send
                    + Sync,
            >;
            let interaction_registrar: PermissionRegistrar = Arc::new(move |request, origin| {
                let conv_public_id = interaction_conv_public_id.clone();
                let registry = interaction_registry.clone();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                let generation = uuid::Uuid::new_v4();
                let safe_input = redact_value(&request.input);
                let entry = PendingPermissionEntry {
                    sender,
                    conv_public_id,
                    kind: request.kind.clone(),
                    tool_name: request.tool_name.clone(),
                    input: safe_input.clone(),
                    generation,
                    origin,
                };
                let inserted = match registry.lock() {
                    Ok(mut pending) => {
                        if pending.contains_key(&request.id) {
                            false
                        } else {
                            pending.insert(request.id.clone(), entry);
                            true
                        }
                    }
                    Err(poisoned) => {
                        let mut pending = poisoned.into_inner();
                        if pending.contains_key(&request.id) {
                            false
                        } else {
                            pending.insert(request.id.clone(), entry);
                            true
                        }
                    }
                };
                if !inserted {
                    return Box::pin(async move {
                        Err(temps_ai::AiError::Provider {
                            purpose: "chat.permission".to_string(),
                            reason: format!(
                                "permission request '{}' is already pending",
                                request.id
                            ),
                        })
                    });
                }

                let guard = PendingPermissionGuard {
                    registry: registry.clone(),
                    permission_id: request.id.clone(),
                    generation,
                };
                Box::pin(async move {
                    let result = receiver.await.map_err(|_| temps_ai::AiError::Provider {
                        purpose: "chat.permission".to_string(),
                        reason: format!(
                            "permission request '{}' ended before it was resolved",
                            request.id
                        ),
                    });
                    drop(guard);
                    result
                })
            });
            let provider_interaction_registrar = interaction_registrar.clone();
            let provider_auto_approve = task_auto_approve_provider_tools.clone();
            let provider_auto_auth = auth.clone();
            let provider_auto_db = db.clone();
            let provider_auto_checker = project_access_checker.clone();
            let provider_auto_application_id = application_id;
            let provider_auto_project_ids = harness_project_ids.clone();
            let provider_auto_provider = ai_provider.clone();
            let provider_auto_permission_mode = task_permission_mode.clone();
            let provider_auto_uses_harness = harness_workspace.is_some();
            let interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
                if provider_auto_approve.load(Ordering::Acquire)
                    && request.kind == PermissionKind::ToolApproval
                {
                    let auth = provider_auto_auth.clone();
                    let db = provider_auto_db.clone();
                    let checker = provider_auto_checker.clone();
                    let project_ids = provider_auto_project_ids.clone();
                    let provider = provider_auto_provider.clone();
                    let permission_mode = active_permission_mode(&provider_auto_permission_mode);
                    return Box::pin(async move {
                        let refreshed = if provider_auto_uses_harness {
                            refresh_harness_authorization(
                                db.as_ref(),
                                &auth,
                                checker.as_ref(),
                                provider_auto_application_id,
                                &project_ids,
                                &provider,
                                &permission_mode,
                            )
                            .await
                        } else {
                            refresh_tool_authorization(db.as_ref(), &auth).await
                        };
                        refreshed
                            .map_err(|error| {
                                tracing::warn!(%error, "denied native tool auto-approval after authorization refresh failed");
                                temps_ai::AiError::Provider {
                                    purpose: "chat.permission.authorization".to_string(),
                                    reason: "the initiating credential is no longer authorized"
                                        .to_string(),
                                }
                            })?;
                        Ok(PermissionDecision::AllowTool)
                    });
                }
                let request_kind = request.kind.clone();
                let pending =
                    provider_interaction_registrar(request, PendingPermissionOrigin::Provider);
                // The mode may have switched to Auto between the first policy
                // check and registration. Re-check after the waiter exists:
                // either the mode-change path already claimed and resolved it,
                // or dropping this exact-generation future removes it before
                // returning the automatic decision. This closes the gap where
                // a newly registered request could otherwise be stranded.
                if provider_auto_approve.load(Ordering::Acquire)
                    && request_kind == PermissionKind::ToolApproval
                {
                    drop(pending);
                    let auth = provider_auto_auth.clone();
                    let db = provider_auto_db.clone();
                    let checker = provider_auto_checker.clone();
                    let project_ids = provider_auto_project_ids.clone();
                    let provider = provider_auto_provider.clone();
                    let permission_mode = active_permission_mode(&provider_auto_permission_mode);
                    return Box::pin(async move {
                        let refreshed = if provider_auto_uses_harness {
                            refresh_harness_authorization(
                                db.as_ref(),
                                &auth,
                                checker.as_ref(),
                                provider_auto_application_id,
                                &project_ids,
                                &provider,
                                &permission_mode,
                            )
                            .await
                        } else {
                            refresh_tool_authorization(db.as_ref(), &auth).await
                        };
                        refreshed
                            .map_err(|error| {
                                tracing::warn!(%error, "denied native tool auto-approval after authorization refresh failed");
                                temps_ai::AiError::Provider {
                                    purpose: "chat.permission.authorization".to_string(),
                                    reason: "the initiating credential is no longer authorized"
                                        .to_string(),
                                }
                            })?;
                        Ok(PermissionDecision::AllowTool)
                    });
                }
                Box::pin(async move { Ok(pending.await?.decision) })
            });
            let sandbox_interaction_tx = tx.clone();
            let sandbox_interaction_live = turn_live.clone();
            let sandbox_interaction_registry = interactions.clone();
            let sandbox_auto_approve = task_auto_approve_provider_tools.clone();
            let sandbox_interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
                // Calling the registry closure synchronously installs the
                // waiter before the event becomes visible, preventing a fast
                // approval click from racing a missing permission id. The
                // underlying interaction performs the authorization refresh
                // even when Auto mode will approve without showing a card.
                let pending = sandbox_interaction_registry(request.clone());
                // `interactions` performs its own post-registration policy
                // check. Mirror it here so an approval that was automatically
                // consumed during that race is never rendered as a ghost card.
                if sandbox_auto_approve.load(Ordering::Acquire)
                    && request.kind == PermissionKind::ToolApproval
                {
                    return pending;
                }
                let safe_input = redact_value(&request.input);
                emit_turn_event(
                    &sandbox_interaction_tx,
                    &sandbox_interaction_live,
                    Ok(ChatStreamEvent::PermissionRequested {
                        id: request.id,
                        kind: request.kind,
                        tool_name: request.tool_name,
                        input: safe_input,
                    }),
                );
                pending
            });
            let platform_interaction_registrar = interaction_registrar.clone();
            let platform_interaction_tx = tx.clone();
            let platform_interaction_live = turn_live.clone();
            let platform_auto_approve = task_auto_approve_provider_tools.clone();
            let platform_auto_auth = auth.clone();
            let platform_auto_metadata = request_metadata.clone();
            let platform_auto_db = db.clone();
            let platform_autofix_incident = autofix_incident.clone();
            let platform_interactions: PlatformInteractionExecutor = Arc::new(move |request| {
                if let Some(incident_public_id) = platform_autofix_incident.clone() {
                    let auth = platform_auto_auth.clone();
                    let metadata = platform_auto_metadata.clone();
                    let db = platform_auto_db.clone();
                    let input = redact_value(&request.input);
                    let tool_name = request.tool_name.clone();
                    return Box::pin(async move {
                        crate::autofix::record_refused_action(
                            db.as_ref(),
                            &incident_public_id,
                            &tool_name,
                            input,
                        )
                        .await;
                        Ok(PermissionResolution {
                            decision: PermissionDecision::DenyTool {
                                reason: Some(crate::autofix::REFUSED_WRITE_REASON.to_string()),
                            },
                            auth,
                            metadata,
                        })
                    });
                }
                if platform_auto_approve.load(Ordering::Acquire) {
                    if let Some(decision) = automatic_platform_decision(&request) {
                        let auth = platform_auto_auth.clone();
                        let metadata = platform_auto_metadata.clone();
                        let db = platform_auto_db.clone();
                        return Box::pin(async move {
                            let auth = refresh_tool_authorization(db.as_ref(), &auth)
                                .await
                                .map_err(|error| {
                                    tracing::warn!(%error, "denied platform auto-approval after authorization refresh failed");
                                    temps_ai::AiError::Provider {
                                        purpose: "chat.permission.authorization".to_string(),
                                        reason: "the initiating credential is no longer authorized"
                                            .to_string(),
                                    }
                                })?;
                            Ok(PermissionResolution {
                                decision,
                                auth,
                                metadata,
                            })
                        });
                    }
                }
                // Registration happens synchronously before the event is
                // published, so a fast click can never race a missing waiter.
                let pending = platform_interaction_registrar(
                    request.clone(),
                    PendingPermissionOrigin::PlatformWrite,
                );
                // Permission mode can change while the request is being
                // registered. The active-mode update may already have claimed
                // it; otherwise dropping this exact-generation waiter removes
                // it before the automatic resolution is returned.
                if platform_auto_approve.load(Ordering::Acquire) {
                    if let Some(decision) = automatic_platform_decision(&request) {
                        drop(pending);
                        let auth = platform_auto_auth.clone();
                        let metadata = platform_auto_metadata.clone();
                        let db = platform_auto_db.clone();
                        return Box::pin(async move {
                            let auth = refresh_tool_authorization(db.as_ref(), &auth)
                                .await
                                .map_err(|error| {
                                    tracing::warn!(%error, "denied platform auto-approval after authorization refresh failed");
                                    temps_ai::AiError::Provider {
                                        purpose: "chat.permission.authorization".to_string(),
                                        reason: "the initiating credential is no longer authorized"
                                            .to_string(),
                                    }
                                })?;
                            Ok(PermissionResolution {
                                decision,
                                auth,
                                metadata,
                            })
                        });
                    }
                }
                let safe_input = redact_value(&request.input);
                emit_turn_event(
                    &platform_interaction_tx,
                    &platform_interaction_live,
                    Ok(ChatStreamEvent::PermissionRequested {
                        id: request.id,
                        kind: request.kind,
                        tool_name: request.tool_name,
                        input: safe_input,
                    }),
                );
                pending
            });
            let executor_state = execution_state.clone();
            let principal_id = auth.user_id();
            let executor_provider = provider.clone();
            let executor_api_tools = api_tools.clone();
            let executor_repo_tools = repo_tools.clone();
            let executor_write_handle = write_handle_opt.clone();
            let executor_pending = pending_svc_opt.clone();
            let executor_audit = write_support_audit.clone();
            let executor_interactions = platform_interactions.clone();
            let executor_context_id = context_id.clone();
            let executor_auth = auth.clone();
            let executor_db = db.clone();
            let executor_project_access_checker = project_access_checker.clone();
            let turn_tool_executor: ToolExecutor = Arc::new(move |call: ToolCall| {
                let state = executor_state.clone();
                let provider = executor_provider.clone();
                let api_tools = executor_api_tools.clone();
                let repo_tools = executor_repo_tools.clone();
                let write_handle = executor_write_handle.clone();
                let pending = executor_pending.clone();
                let audit = executor_audit.clone();
                let interactions = executor_interactions.clone();
                let context_id = executor_context_id.clone();
                let auth = executor_auth.clone();
                let db = executor_db.clone();
                let project_access_checker = executor_project_access_checker.clone();
                Box::pin(async move {
                    // Incident refusals happen inside `dispatch_conversation_tool`
                    // (via `ToolExecutionState::incident`), shared with the
                    // gateway rounds loop.
                    let auth = match refresh_tool_authorization(db.as_ref(), &auth).await {
                        Ok(auth) => auth,
                        Err(error) => {
                            tracing::warn!(
                                conversation_id = conv_id,
                                tool = %call.name,
                                %error,
                                "denied conversation tool after authorization refresh failed"
                            );
                            return Err(temps_ai::AiError::Provider {
                                purpose: "chat.tool.authorization".to_string(),
                                reason: "the initiating credential is no longer authorized"
                                    .to_string(),
                            });
                        }
                    };
                    let mut state = state.lock().await;
                    Ok(dispatch_conversation_tool(
                        &call,
                        project_id,
                        conv_id,
                        &context_id,
                        &auth,
                        project_access_checker.as_ref(),
                        provider.as_ref(),
                        api_tools.as_ref(),
                        repo_tools.as_ref(),
                        write_handle.as_deref(),
                        pending.as_deref(),
                        audit.as_deref(),
                        Some(&interactions),
                        &mut state,
                    )
                    .await)
                })
            });
            let process_redaction_values = sandbox_environment
                .redaction_values()
                .cloned()
                .collect::<Vec<_>>();
            let process_executor: Option<ManagedProcessExecutor> =
                harness_workspace.as_ref().map(|workspace| {
                    let ai = ai.clone();
                    let workspace = workspace.clone();
                    let provider = ai_provider.clone();
                    let auth = auth.clone();
                    let db = db.clone();
                    let checker = project_access_checker.clone();
                    let project_ids = harness_project_ids.clone();
                    let interactions = sandbox_interactions.clone();
                    let permission_mode = task_permission_mode.clone();
                    let redaction_values = process_redaction_values.clone();
                    Arc::new(move |call: ToolCall| -> BoxFuture<'static, Result<String, temps_ai::AiError>> {
                        let ai = ai.clone();
                        let workspace = workspace.clone();
                        let provider = provider.clone();
                        let auth = auth.clone();
                        let db = db.clone();
                        let checker = checker.clone();
                        let project_ids = project_ids.clone();
                        let interactions = interactions.clone();
                        let permission_mode = permission_mode.clone();
                        let redaction_values = redaction_values.clone();
                        Box::pin(async move {
                            let operation = parse_runtime_process_operation(&call)?;
                            let mode = active_permission_mode(&permission_mode);
                            refresh_harness_authorization(
                                db.as_ref(),
                                &auth,
                                checker.as_ref(),
                                application_id,
                                &project_ids,
                                &provider,
                                &mode,
                            )
                            .await
                            .map_err(|error| {
                                tracing::warn!(%error, tool = %call.name, "denied managed process operation after authorization refresh failed");
                                temps_ai::AiError::Provider {
                                    purpose: "chat.runtime_process.authorization".to_string(),
                                    reason: "the initiating credential is no longer authorized"
                                        .to_string(),
                                }
                            })?;

                            if matches!(
                                operation,
                                RuntimeProcessOperation::Start { .. }
                                    | RuntimeProcessOperation::Stop { .. }
                                    | RuntimeProcessOperation::Restart { .. }
                            ) {
                                let permission_input: serde_json::Value =
                                    serde_json::from_str(&call.arguments).map_err(|_| {
                                        invalid_process_request(
                                            "the managed process permission input is invalid",
                                        )
                                    })?;
                                let decision = (interactions)(PermissionRequest {
                                    id: uuid::Uuid::new_v4().simple().to_string(),
                                    kind: PermissionKind::ToolApproval,
                                    tool_name: call.name.clone(),
                                    input: redact_value(&permission_input),
                                })
                                .await?;
                                if !matches!(decision, PermissionDecision::AllowTool) {
                                    return Err(temps_ai::AiError::Provider {
                                        purpose: "chat.runtime_process.permission".to_string(),
                                        reason: "the managed process operation was not approved"
                                            .to_string(),
                                    });
                                }
                                let mode = active_permission_mode(&permission_mode);
                                refresh_harness_authorization(
                                    db.as_ref(),
                                    &auth,
                                    checker.as_ref(),
                                    application_id,
                                    &project_ids,
                                    &provider,
                                    &mode,
                                )
                                .await
                                .map_err(|error| {
                                    tracing::warn!(%error, tool = %call.name, "denied approved managed process operation after authorization changed");
                                    temps_ai::AiError::Provider {
                                        purpose: "chat.runtime_process.authorization".to_string(),
                                        reason:
                                            "the initiating credential is no longer authorized"
                                                .to_string(),
                                    }
                                })?;
                            }

                            let response = tokio::time::timeout(
                                Duration::from_secs(30),
                                ai.runtime_process(RuntimeProcessRequest {
                                    principal_id,
                                    provider,
                                    harness_workspace: workspace,
                                    operation,
                                }),
                            )
                            .await
                            .map_err(|_| temps_ai::AiError::Provider {
                                purpose: "chat.runtime_process".to_string(),
                                reason: "the managed process operation timed out".to_string(),
                            })?
                            .map_err(|error| {
                                sanitize_runtime_process_error(error, &redaction_values)
                            })?;
                            serialize_process_result(&response, &redaction_values)
                        })
                    }) as ManagedProcessExecutor
                });
            let (harness_mcp_server, _harness_mcp_guard, mut harness_mcp_event_rx) = match (
                harness_workspace.as_ref(),
                harness_internal_api_url.as_deref(),
            ) {
                (Some(_), Some(internal_api_url)) => {
                    let mut harness_tools = tools.clone();
                    harness_tools.extend(managed_process_tools());
                    let (server, guard, event_rx) = ConversationService::register_harness_mcp(
                        harness_mcp_entries,
                        internal_api_url,
                        principal_id,
                        harness_tools,
                        turn_tool_executor.clone(),
                        process_executor,
                        sandbox_interactions.clone(),
                        max_turn_duration + Duration::from_secs(30),
                    );
                    (Some(server), Some(guard), Some(event_rx))
                }
                _ => (None, None, None),
            };
            // Structured record of each executed tool, persisted on the assistant
            // message's metadata so the chat replays its tool work after a reload.
            let mut tools_meta: Vec<serde_json::Value> = Vec::new();
            // Ordered render segments (text / tool, in occurrence order). Persisted
            // so a reload shows the same interleaving the live stream did.
            let mut parts: Vec<serde_json::Value> = Vec::new();
            // The open text segment being accumulated (flushed into `parts` when a
            // tool call interrupts it or the turn ends).
            let mut cur_text = String::new();
            // All assistant prose across the turn — the persisted `content` and the
            // history replayed to the model on the next turn.
            let mut content = String::new();
            // Text deltas can be very small. Checkpoint immediately, then at
            // most four times per second; structural events always checkpoint.
            let mut last_draft_checkpoint: Option<Instant> = None;
            // Did a round answer in prose (no tool calls)? Then we have the final
            // answer and stop; otherwise we may need a salvage call.
            let mut answered = false;
            // The last provider error seen while trying to produce this turn. Kept
            // so that a turn which ends up with nothing to show can explain WHY
            // instead of just stopping — see the empty-turn check after salvage.
            let mut last_provider_error: Option<String> = None;
            let mut retained_diagnostic: Option<(String, String)> = None;
            let mut durable_failure: Option<crate::PublicChatFailure> = None;
            // Set when the provider's reply stream ended before the provider
            // said it was finished (dropped connection, mid-reply error, client
            // timeout, no terminal chunk). The text received so far is partial:
            // the turn is failed, not completed.
            let mut stream_interruption: Option<String> = None;
            let mut provider_session_id: Option<String> = None;
            let mut provider_session_title: Option<String> = None;
            let mut resume_session_id = resume_session_id;
            // A new turn with no durable provider session must not attach a
            // runtime that still carries an older, now-missing session.
            let mut reset_retained_session =
                harness_workspace.is_some() && resume_session_id.is_none();
            let mut missing_session_retry_used = false;
            // Why generation stopped, when it was a bound rather than the model
            // finishing. Reported to the user — a turn that halts for a reason
            // nobody states looks identical to one that simply gave up.
            let mut stop_reason: Option<&'static str> = None;
            let mut budget = TurnBudget::start(round_limits, max_turn_duration);
            let trace_id = task_turn_id.as_deref().unwrap_or("untracked");
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = trace_id,
                conversation_id = conv_id,
                project_id,
                provider = %ai_provider,
                phase = "execution_loop_started",
                total_ms = 0_u64,
                "AI turn timing"
            );

            'rounds: for round_index in 0..budget.max_rounds() {
                if budget.deadline_reached() {
                    stop_reason = Some(TURN_TIMEOUT_REASON);
                    break 'rounds;
                }
                if harness_workspace.is_some() {
                    let permission_mode = active_permission_mode(&task_permission_mode);
                    if let Err(error) = refresh_harness_authorization(
                        db.as_ref(),
                        &auth,
                        project_access_checker.as_ref(),
                        application_id,
                        &harness_project_ids,
                        &ai_provider,
                        &permission_mode,
                    )
                    .await
                    {
                        tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "stopped AI turn before provider execution because authorization changed"
                        );
                        emit_turn_event(
                            &tx,
                            &turn_live,
                            Err(ChatError::AuthorizationRefresh(error)),
                        );
                        return true;
                    }
                }
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools"),
                    conversation_id: Some(conv_id.to_string()),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: resume_session_id.clone(),
                    reset_retained_session,
                    messages: messages.clone(),
                    sandbox_attachments: sandbox_attachments.clone(),
                    tools: tools.clone(),
                    harness_workspace: harness_workspace.clone(),
                    sandbox_environment: sandbox_environment.clone(),
                    harness_mcp_server: harness_mcp_server.clone(),
                    capture_session_title: should_capture_session_title,
                    ..Default::default()
                };
                // A single streaming pass: text deltas and tool calls arrive
                // inline. An error here (e.g. the model can't do tools) ends the
                // loop; the salvage below still tries a tool-free reply.
                let provider_call_started = tokio::time::Instant::now();
                reset_retained_session = false;
                let mut stream = match ai
                    .chat_stream_turn_with_services(
                        req,
                        temps_ai::TurnServices {
                            tools: Some(turn_tool_executor.clone()),
                            interactions: Some(interactions.clone()),
                        },
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::info!(
                        component = "ai_turn_timing",
                                        turn_id = trace_id,
                                        conversation_id = conv_id,
                                        project_id,
                                        provider = %ai_provider,
                                        phase = "provider_stream_failed",
                                        round = round_index + 1,
                                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                                        total_ms = budget.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                        let reason = e.to_string();
                        if let temps_ai::AiError::RetainedHarnessDiagnostic { purpose, reason } = &e
                        {
                            retained_diagnostic = Some((purpose.clone(), reason.clone()));
                        }
                        tracing::warn!(
                            "chat_stream_turn failed for conv {conv_id} (round): {reason}"
                        );
                        if can_retry_missing_provider_session(
                            &ai_provider,
                            &reason,
                            resume_session_id.is_some(),
                            missing_session_retry_used,
                            false,
                        ) {
                            tracing::warn!(
                                conversation_id = conv_id,
                                provider = %ai_provider,
                                "provider resume session is missing; rebuilding it from durable conversation history"
                            );
                            resume_session_id = None;
                            reset_retained_session = true;
                            missing_session_retry_used = true;
                            provider_session_id = None;
                            provider_session_title = None;
                            if let Err(error) = clear_missing_provider_session(
                                db.as_ref(),
                                conv_id,
                                task_turn_id.as_deref(),
                            )
                            .await
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    %error,
                                    "failed to clear missing provider session id"
                                );
                            }
                            retained_diagnostic = None;
                            continue 'rounds;
                        }
                        remember_provider_error(&mut last_provider_error, reason);
                        break 'rounds;
                    }
                };
                tracing::info!(
                component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_ready",
                        round = round_index + 1,
                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                        total_ms = budget.elapsed().as_millis() as u64,
                        "AI turn timing"
                    );
                let mut round_text = String::new();
                let mut round_calls: Vec<ToolCall> = Vec::new();
                let mut round_had_effect = false;
                let mut native_tool_ids = std::collections::HashSet::new();
                let mut pending_managed_process_ids = std::collections::HashSet::new();
                let mut provider_stream_finished = false;
                // Did anything this round return usable data (vs. only rejections)?
                let mut round_produced_something = false;
                let mut first_delta_seen = false;
                let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
                authorization_check
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The turn was checked immediately before provider startup.
                // Consume the interval's immediate first tick so subsequent
                // checks happen once per second while native commands run.
                authorization_check.tick().await;
                loop {
                    if provider_stream_finished && pending_managed_process_ids.is_empty() {
                        break;
                    }
                    let (item, authoritative_mcp_event) = tokio::select! {
                        biased;
                        item = async {
                            match harness_mcp_event_rx.as_mut() {
                                Some(receiver) => receiver.recv().await,
                                None => std::future::pending().await,
                            }
                        } => match item {
                            Some(item) => (Ok(item), true),
                            None => {
                                harness_mcp_event_rx = None;
                                if provider_stream_finished {
                                    break;
                                }
                                continue;
                            },
                        },
                        item = stream.next(), if !provider_stream_finished => match item {
                            Some(item) => (item, false),
                            None => {
                                provider_stream_finished = true;
                                if pending_managed_process_ids.is_empty() {
                                    break;
                                }
                                continue;
                            }
                        },
                        _ = tokio::time::sleep(
                            budget.remaining()
                        ), if provider_stream_finished && !pending_managed_process_ids.is_empty() => {
                            tracing::warn!(
                                conversation_id = conv_id,
                                pending_process_tools = pending_managed_process_ids.len(),
                                "stopped waiting for terminal managed process tool events at the turn deadline"
                            );
                            break;
                        }
                        _ = authorization_check.tick(), if harness_workspace.is_some() => {
                            let permission_mode = active_permission_mode(&task_permission_mode);
                            match refresh_harness_authorization(
                                db.as_ref(),
                                &auth,
                                project_access_checker.as_ref(),
                                application_id,
                                &harness_project_ids,
                                &ai_provider,
                                &permission_mode,
                            ).await {
                                Ok(_) => continue,
                                Err(error) => {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "stopped active native harness after authorization changed"
                                    );
                                    emit_turn_event(
                                        &tx,
                                        &turn_live,
                                        Err(ChatError::AuthorizationRefresh(error)),
                                    );
                                    return true;
                                }
                            }
                        }
                    };
                    if !authoritative_mcp_event
                        && matches!(
                            &item,
                            Ok(ChatStreamDelta::ToolCall(call))
                                if is_managed_process_echo(&call.name)
                        )
                    {
                        continue;
                    }
                    if !authoritative_mcp_event
                        && matches!(
                            &item,
                            Ok(ChatStreamDelta::ToolResult { call, .. })
                                if is_managed_process_echo(&call.name)
                        )
                    {
                        continue;
                    }
                    if !first_delta_seen {
                        first_delta_seen = true;
                        let delta_kind = match &item {
                            Ok(ChatStreamDelta::Text(_)) => "text",
                            Ok(ChatStreamDelta::ContextUsage(_)) => "context_usage",
                            Ok(ChatStreamDelta::ToolCall(_)) => "tool_call",
                            Ok(ChatStreamDelta::ToolResult { .. }) => "tool_result",
                            Ok(ChatStreamDelta::PermissionRequested(_)) => "permission",
                            Ok(ChatStreamDelta::SessionMetadata { .. }) => "session_metadata",
                            Err(_) => "error",
                        };
                        tracing::info!(
                        component = "ai_turn_timing",
                                        turn_id = trace_id,
                                        conversation_id = conv_id,
                                        project_id,
                                        provider = %ai_provider,
                                        phase = "provider_first_delta",
                                        round = round_index + 1,
                                        delta_kind,
                                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                                        total_ms = budget.elapsed().as_millis() as u64,
                                        "AI turn timing"
                                    );
                    }
                    match item {
                        Ok(ChatStreamDelta::ContextUsage(usage)) => {
                            let usage = PersistedContextWindowUsage::from(usage);
                            if let Err(error) = persist_context_usage(
                                db.as_ref(),
                                conv_id,
                                task_turn_id.as_deref(),
                                &usage,
                            )
                            .await
                            {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    %error,
                                    "failed to persist provider context usage"
                                );
                            }
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::ContextUsage(usage)),
                            );
                        }
                        Ok(ChatStreamDelta::Text(t)) => {
                            round_had_effect |= !t.is_empty();
                            // Separate this round's prose from anything already shown
                            // (e.g. a previous round's narration) with a blank line.
                            if round_text.is_empty()
                                && !content.is_empty()
                                && !content.ends_with('\n')
                            {
                                let sep = "\n\n".to_string();
                                content.push_str(&sep);
                                cur_text.push_str(&sep);
                                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(sep)));
                            }
                            round_text.push_str(&t);
                            content.push_str(&t);
                            cur_text.push_str(&t);
                            emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(t)));
                            if last_draft_checkpoint
                                .is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
                            {
                                if let Some(message_id) = draft_message_id {
                                    if let Err(error) = persist_assistant_message(
                                        db.as_ref(),
                                        message_id,
                                        &content,
                                        &tools_meta,
                                        &parts,
                                        &cur_text,
                                        true,
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            conv_id,
                                            message_id,
                                            %error,
                                            "failed to checkpoint assistant text"
                                        );
                                    }
                                    last_draft_checkpoint = Some(Instant::now());
                                }
                            }
                        }
                        Ok(ChatStreamDelta::ToolCall(tc)) => {
                            round_had_effect = true;
                            if authoritative_mcp_event && is_managed_process_tool(&tc.name) {
                                pending_managed_process_ids.insert(tc.id.clone());
                            }
                            let display_name =
                                managed_process_display_name(&tc.name, authoritative_mcp_event);
                            // Close any open text part so order is preserved, then
                            // surface the call live (the result follows once it runs).
                            if !cur_text.is_empty() {
                                parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": std::mem::take(&mut cur_text),
                                }));
                            }
                            let display_arguments = redact_json_string(&tc.arguments);
                            record_tool_call(
                                &mut tools_meta,
                                &mut parts,
                                &tc.id,
                                &display_name,
                                &display_arguments,
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant tool call"
                                    );
                                }
                                last_draft_checkpoint = Some(Instant::now());
                            }
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::ToolCall {
                                    id: tc.id.clone(),
                                    name: display_name,
                                    arguments: display_arguments,
                                }),
                            );
                            // A sandbox harness has already executed this
                            // native tool itself. In particular, a background
                            // Bash job may not emit a terminal tool_result
                            // until the process exits. Never hand that call to
                            // the outer API-tool executor while waiting: doing
                            // so replays unknown native tools for up to the
                            // 500-round anti-spin guard and produces the
                            // misleading "unusually long run" fallback.
                            if harness_workspace.is_some() {
                                native_tool_ids.insert(tc.id.clone());
                            }
                            round_calls.push(tc);
                        }
                        Ok(ChatStreamDelta::ToolResult { call, result }) => {
                            round_had_effect = true;
                            if authoritative_mcp_event && is_managed_process_tool(&call.name) {
                                pending_managed_process_ids.remove(&call.id);
                            }
                            let display_name =
                                managed_process_display_name(&call.name, authoritative_mcp_event);
                            native_tool_ids.insert(call.id.clone());
                            let display_arguments = redact_json_string(&call.arguments);
                            let display_result = redact_json_string(&result);
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::ToolResult {
                                    id: call.id.clone(),
                                    name: display_name.clone(),
                                    content: display_result.clone(),
                                }),
                            );
                            record_tool_result(
                                &mut tools_meta,
                                &mut parts,
                                &call.id,
                                &display_name,
                                &display_arguments,
                                &display_result,
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant tool result"
                                    );
                                }
                                last_draft_checkpoint = Some(Instant::now());
                            }
                            round_produced_something = true;
                        }
                        Ok(ChatStreamDelta::PermissionRequested(perm)) => {
                            round_had_effect = true;
                            // The gateway AI path (OpenAI/Anthropic API) never emits
                            // this — it only comes from `run_interactive` on the
                            // interactive CLI path, which has its own streaming channel.
                            // If it somehow arrives here, forward it to the client
                            // (harmless) and log so operators can investigate.
                            tracing::warn!(
                                conv_id,
                                permission_id = %perm.id,
                                tool_name = %perm.tool_name,
                                "PermissionRequested delta arrived in the gateway tool loop \
                                 (unexpected — only expected on the interactive CLI path); \
                                 forwarding to client"
                            );
                            if let Some(message_id) = draft_message_id {
                                if let Err(error) = persist_assistant_message(
                                    db.as_ref(),
                                    message_id,
                                    &content,
                                    &tools_meta,
                                    &parts,
                                    &cur_text,
                                    true,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conv_id,
                                        message_id,
                                        %error,
                                        "failed to checkpoint assistant before permission request"
                                    );
                                }
                            }
                            emit_turn_event(
                                &tx,
                                &turn_live,
                                Ok(ChatStreamEvent::PermissionRequested {
                                    id: perm.id,
                                    kind: perm.kind,
                                    tool_name: perm.tool_name,
                                    input: perm.input,
                                }),
                            );
                        }
                        Ok(ChatStreamDelta::SessionMetadata { session_id, title }) => {
                            if session_id.is_some() {
                                provider_session_id = session_id;
                            }
                            if title.is_some() {
                                provider_session_title = title;
                            }
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            if let temps_ai::AiError::RetainedHarnessDiagnostic {
                                purpose,
                                reason,
                            } = &e
                            {
                                retained_diagnostic = Some((purpose.clone(), reason.clone()));
                            }
                            if let temps_ai::AiError::StreamInterrupted { reason, .. } = &e {
                                stream_interruption = Some(reason.clone());
                            }
                            tracing::warn!(
                                "chat_stream_turn item error for conv {conv_id}: {reason}"
                            );
                            if can_retry_missing_provider_session(
                                &ai_provider,
                                &reason,
                                resume_session_id.is_some(),
                                missing_session_retry_used,
                                round_had_effect,
                            ) {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    provider = %ai_provider,
                                    "provider resume session is missing; rebuilding it from durable conversation history"
                                );
                                resume_session_id = None;
                                reset_retained_session = true;
                                missing_session_retry_used = true;
                                provider_session_id = None;
                                provider_session_title = None;
                                if let Err(error) = clear_missing_provider_session(
                                    db.as_ref(),
                                    conv_id,
                                    task_turn_id.as_deref(),
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "failed to clear missing provider session id"
                                    );
                                }
                                retained_diagnostic = None;
                                stream_interruption = None;
                                continue 'rounds;
                            }
                            // Provider subprocess failures arrive as stream items
                            // after `chat_stream_turn_with_executor` has returned.
                            // Preserve the concrete reason so an empty turn reports
                            // the authentication/model error instead of the generic
                            // "provider returned no response" fallback.
                            remember_provider_error(&mut last_provider_error, reason);
                            break;
                        }
                    }
                }
                tracing::info!(
                component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_complete",
                        round = round_index + 1,
                        phase_ms = provider_call_started.elapsed().as_millis() as u64,
                        total_ms = budget.elapsed().as_millis() as u64,
                        failed = stream_interruption.is_some(),
                        "AI turn timing"
                    );

                if let Some(cause) = stream_interruption.as_deref() {
                    // Do not execute tool calls from, or ask for another round
                    // after, a reply the provider never finished.
                    tracing::warn!(
                        component = "ai_turn_timing",
                        turn_id = trace_id,
                        conversation_id = conv_id,
                        project_id,
                        provider = %ai_provider,
                        phase = "provider_stream_interrupted",
                        round = round_index + 1,
                        partial_bytes = content.len() + round_text.len(),
                        failed = true,
                        cause,
                        "AI provider stream ended before the reply was complete"
                    );
                    break 'rounds;
                }

                if !native_tool_ids.is_empty() {
                    round_calls.retain(|call| !native_tool_ids.contains(&call.id));
                    if round_calls.is_empty() {
                        answered = !round_text.is_empty();
                        break 'rounds;
                    }
                }

                if round_calls.is_empty() {
                    // The model answered in prose — that streamed text is the final
                    // answer. Done.
                    answered = true;
                    break 'rounds;
                }

                // Record the assistant's tool-call turn for the next round's context.
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: round_text,
                    tool_calls: Some(round_calls.clone()),
                    tool_call_id: None,
                    images: Vec::new(),
                });
                for (call_index, tc) in round_calls.iter().enumerate() {
                    // Route the ADR-024 `temps` CLI tool to the API-tools provider;
                    // `temps_write` to the write-proposal path;
                    // otherwise to the context provider. `project_id` is always the
                    // conversation's project, never anything the model supplied — so
                    // a tool can't be steered to another tenant's data.
                    let result = if call_index >= budget.max_calls_per_round() {
                        // Answer the call rather than dropping it: a provider rejects an
                        // assistant tool_calls block with unanswered ids, and the model
                        // needs to know the call did not run.
                        tool_call_cap_message(budget.max_calls_per_round())
                    } else {
                        match refresh_tool_authorization(db.as_ref(), &auth).await {
                            Ok(fresh_auth) => {
                                let mut state = execution_state.lock().await;
                                dispatch_conversation_tool(
                                    tc,
                                    project_id,
                                    conv_id,
                                    &context_id,
                                    &fresh_auth,
                                    project_access_checker.as_ref(),
                                    provider.as_ref(),
                                    api_tools.as_ref(),
                                    repo_tools.as_ref(),
                                    write_handle_opt.as_deref(),
                                    pending_svc_opt.as_deref(),
                                    write_support_audit.as_deref(),
                                    Some(&platform_interactions),
                                    &mut state,
                                )
                                .await
                            }
                            Err(error) => {
                                tracing::warn!(
                                    conversation_id = conv_id,
                                    tool = %tc.name,
                                    %error,
                                    "denied conversation tool after authorization refresh failed"
                                );
                                emit_turn_event(
                                    &tx,
                                    &turn_live,
                                    Err(ChatError::AuthorizationRefresh(error)),
                                );
                                return true;
                            }
                        }
                    };
                    let display_arguments = redact_json_string(&tc.arguments);
                    let display_result = public_tool_result(&tc.name, &result);
                    // Surface the result right after — live.
                    emit_turn_event(
                        &tx,
                        &turn_live,
                        Ok(ChatStreamEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            content: display_result.clone(),
                        }),
                    );
                    record_tool_result(
                        &mut tools_meta,
                        &mut parts,
                        &tc.id,
                        &tc.name,
                        &display_arguments,
                        &display_result,
                    );
                    if let Some(message_id) = draft_message_id {
                        if let Err(error) = persist_assistant_message(
                            db.as_ref(),
                            message_id,
                            &content,
                            &tools_meta,
                            &parts,
                            &cur_text,
                            true,
                        )
                        .await
                        {
                            tracing::warn!(
                                conv_id,
                                message_id,
                                %error,
                                "failed to checkpoint dispatched tool result"
                            );
                        }
                        last_draft_checkpoint = Some(Instant::now());
                    }
                    if tool_result_is_productive(&result) {
                        round_produced_something = true;
                    }
                    messages.push(ChatMessage::tool(tc.id.clone(), result));
                }

                if let Some(reason) = budget.record_round(round_produced_something) {
                    stop_reason = Some(reason);
                    break 'rounds;
                }

                // Bound what gets replayed next round. This, not the round
                // count, is what keeps a long turn from running out of context.
                trim_carried_tool_results(&mut messages, MAX_CARRIED_TOOL_BYTES);
                // Warn the model before the step cap cuts it off, so work still
                // in flight (a deployment building) gets watched instead of
                // promised. It rides on the round's last tool result, which the
                // model reads next and the user never sees.
                if let Some(note) = budget.steps_left_note(round_index) {
                    if let Some(last) = messages.last_mut().filter(|m| m.role == "tool") {
                        last.content.push_str(note);
                    }
                }
                budget.round_completed(round_index);
            }

            // Fell out of the loop by exhausting the backstop rather than by
            // answering — worth saying, since it means the task was cut short.
            if budget.backstop_exhausted()
                && !answered
                && stop_reason.is_none()
                && !tools_meta.is_empty()
            {
                stop_reason = Some(ROUND_BACKSTOP_REASON);
            }

            // Close any trailing open text part.
            if !cur_text.is_empty() {
                parts.push(serde_json::json!({
                    "type": "text",
                    "text": std::mem::take(&mut cur_text),
                }));
            }

            // Say that the turn was cut short, before the salvage answer.
            //
            // Without this the user gets a confident-looking summary with no
            // indication it was written under duress — which reads as a complete
            // answer and is the same class of dishonesty as claiming a backtest
            // that never ran. They can then decide to ask it to continue.
            if let Some(reason) = stop_reason {
                let note = format!(
                    "\n\n_The assistant {reason}. What follows is based only on \
                     what it gathered so far — ask it to continue if that looks incomplete._\n\n"
                );
                content.push_str(&note);
                parts.push(serde_json::json!({ "type": "text", "text": note.clone() }));
                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(note)));
            }

            // Salvage: the loop used tools but never settled on a prose answer (it
            // hit a bound while still calling tools). Make one tool-free streaming
            // call so the model answers from the evidence it gathered. The result
            // is persisted even when no browser is currently attached.
            if !answered && !tools_meta.is_empty() && stream_interruption.is_none() {
                if harness_workspace.is_some() {
                    let permission_mode = active_permission_mode(&task_permission_mode);
                    if let Err(error) = refresh_harness_authorization(
                        db.as_ref(),
                        &auth,
                        project_access_checker.as_ref(),
                        application_id,
                        &harness_project_ids,
                        &ai_provider,
                        &permission_mode,
                    )
                    .await
                    {
                        tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "stopped salvage response because authorization changed"
                        );
                        emit_turn_event(
                            &tx,
                            &turn_live,
                            Err(ChatError::AuthorizationRefresh(error)),
                        );
                        return true;
                    }
                }
                let mut final_messages = messages;
                final_messages.push(ChatMessage::user(FINAL_DIRECTIVE));
                let req = ChatTurnRequest {
                    trace_id: task_turn_id.clone(),
                    purpose: format!("chat.{context_type}.tools.final"),
                    conversation_id: Some(conv_id.to_string()),
                    project_id,
                    principal_id: Some(principal_id),
                    provider: Some(ai_provider.clone()),
                    model: Some(ai_model.clone()),
                    thinking_level: ai_thinking_level.clone(),
                    permission_mode: Some(ai_permission_mode.clone()),
                    resume_session_id: resume_session_id.clone(),
                    messages: final_messages,
                    sandbox_attachments,
                    harness_workspace: harness_workspace.clone(),
                    sandbox_environment: sandbox_environment.clone(),
                    ..Default::default()
                };
                let salvage = ai
                    .chat_stream_turn_with_services(
                        req,
                        temps_ai::TurnServices {
                            tools: None,
                            interactions: Some(interactions.clone()),
                        },
                    )
                    .await;
                if let Err(e) = &salvage {
                    if let temps_ai::AiError::RetainedHarnessDiagnostic { purpose, reason } = e {
                        retained_diagnostic = Some((purpose.clone(), reason.clone()));
                    }
                    tracing::warn!("chat_stream_turn failed for conv {conv_id} (salvage): {e}");
                    remember_provider_error(&mut last_provider_error, e.to_string());
                }
                if let Ok(mut stream) = salvage {
                    let mut salvage_text = String::new();
                    let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
                    authorization_check
                        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    authorization_check.tick().await;
                    loop {
                        let item = tokio::select! {
                            item = stream.next() => match item {
                                Some(item) => item,
                                None => break,
                            },
                            _ = authorization_check.tick(), if harness_workspace.is_some() => {
                                let permission_mode = active_permission_mode(&task_permission_mode);
                                match refresh_harness_authorization(
                                    db.as_ref(),
                                    &auth,
                                    project_access_checker.as_ref(),
                                    application_id,
                                    &harness_project_ids,
                                    &ai_provider,
                                    &permission_mode,
                                ).await {
                                    Ok(_) => continue,
                                    Err(error) => {
                                        tracing::warn!(
                                            conversation_id = conv_id,
                                            %error,
                                            "stopped active salvage harness after authorization changed"
                                        );
                                        emit_turn_event(
                                            &tx,
                                            &turn_live,
                                            Err(ChatError::AuthorizationRefresh(error)),
                                        );
                                        return true;
                                    }
                                }
                            }
                        };
                        match item {
                            Ok(ChatStreamDelta::Text(t)) => {
                                if salvage_text.is_empty()
                                    && !content.is_empty()
                                    && !content.ends_with('\n')
                                {
                                    let sep = "\n\n".to_string();
                                    content.push_str(&sep);
                                    emit_turn_event(
                                        &tx,
                                        &turn_live,
                                        Ok(ChatStreamEvent::Token(sep)),
                                    );
                                }
                                salvage_text.push_str(&t);
                                content.push_str(&t);
                                emit_turn_event(&tx, &turn_live, Ok(ChatStreamEvent::Token(t)));
                            }
                            Ok(ChatStreamDelta::ContextUsage(usage)) => {
                                let usage = PersistedContextWindowUsage::from(usage);
                                if let Err(error) = persist_context_usage(
                                    db.as_ref(),
                                    conv_id,
                                    task_turn_id.as_deref(),
                                    &usage,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        conversation_id = conv_id,
                                        %error,
                                        "failed to persist salvage context usage"
                                    );
                                }
                                emit_turn_event(
                                    &tx,
                                    &turn_live,
                                    Ok(ChatStreamEvent::ContextUsage(usage)),
                                );
                            }
                            Ok(_) => {}
                            Err(e) => {
                                // The salvage answer is the turn's final text; a
                                // stream cut short here must fail the turn just
                                // like one cut short in a tool round.
                                if let temps_ai::AiError::StreamInterrupted { reason, .. } = &e {
                                    stream_interruption = Some(reason.clone());
                                }
                                tracing::warn!(
                                    "chat_stream_turn item error for conv {conv_id} (salvage): {e}"
                                );
                                remember_provider_error(&mut last_provider_error, e.to_string());
                                break;
                            }
                        }
                    }
                    if !salvage_text.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": salvage_text }));
                    }
                }
            }

            // The write contract is receipt-backed. A model can see an older
            // proposal in conversation history and incorrectly describe it as a
            // new one without calling the tool again. Never persist that claim as
            // success: there is no durable action for the UI to render or confirm.
            let proposal_not_staged =
                claims_proposal_was_staged(&content) && !has_fresh_proposal_receipt(&tools_meta);
            if proposal_not_staged {
                tracing::warn!(
                    conv_id,
                    "assistant claimed a proposal was staged without a current-turn receipt"
                );
                const CORRECTION: &str = "Temps could not verify this proposal because the AI did not submit it through the write tool. No approval card was created and no change was made. Retry to create a fresh proposal.";
                content = CORRECTION.to_string();
                // Keep attempted tool calls for diagnosis, but discard the
                // misleading prose and replace it with the server-owned truth.
                parts.retain(|part| {
                    part.get("type").and_then(serde_json::Value::as_str) == Some("tool")
                });
                parts.push(serde_json::json!({ "type": "text", "text": CORRECTION }));
                emit_turn_event(&tx, &turn_live, Err(ChatError::ProposalNotStaged));
                durable_failure = Some(ChatError::ProposalNotStaged.public_failure());
            }

            // A turn that produced nothing at all — no prose, no tool work — is
            // indistinguishable from a hung request in the UI: the user's own
            // message sits there and nothing ever arrives. That is the worst
            // possible outcome for a self-hosted operator with no support channel
            // to ask, so say what went wrong. The client already renders an
            // `error` SSE event; before this it simply never received one, because
            // a provider failure just ended the loop.
            //
            let empty_turn_failed = content.is_empty() && tools_meta.is_empty();
            let turn_failed = proposal_not_staged
                || empty_turn_failed
                || retained_diagnostic.is_some()
                || stream_interruption.is_some();
            let provider_stopped_early = last_provider_error.is_some() || stop_reason.is_some();
            // A reply the provider never finished is a failed turn even when
            // text arrived: persisting that text as a finished answer is how a
            // truncated reply used to be shown with no error. The text is kept,
            // flagged as partial, and the failure is both emitted and persisted.
            let mut interrupted_message: Option<crate::PublicChatFailure> = None;
            if let Some(cause) = stream_interruption.take() {
                let error = ChatError::StreamInterrupted {
                    conversation_id: conv_id,
                    turn_id: trace_id.to_string(),
                    partial_bytes: content.len(),
                    cause,
                };
                let failure = error.public_failure();
                tracing::warn!(
                    component = "ai_turn_timing",
                    turn_id = trace_id,
                    conversation_id = conv_id,
                    failed = true,
                    %error,
                    "AI turn failed: provider stream interrupted"
                );
                emit_turn_event(&tx, &turn_live, Err(error));
                durable_failure.get_or_insert(failure.clone());
                interrupted_message = Some(failure);
            }
            if empty_turn_failed && interrupted_message.is_some() {
                // Already reported with the precise cause above; do not follow
                // it with the generic "no reply was produced" error.
            } else if empty_turn_failed {
                // `ChatError::Ai` already renders an "AI provider error: " prefix,
                // so the message continues that sentence rather than restating it.
                let detail = last_provider_error
                    .unwrap_or_else(|| "the provider returned no response".to_string());
                let error = if let Some((purpose, reason)) = retained_diagnostic.take() {
                    ChatError::RetainedHarnessDiagnostic { purpose, reason }
                } else {
                    ChatError::Ai(format!(
                        "no reply was produced. {detail} \
                         Review the concrete error and the selected harness/provider status, \
                         then try again."
                    ))
                };
                durable_failure = Some(error.public_failure());
                emit_turn_event(&tx, &turn_live, Err(error));
            }
            if !empty_turn_failed {
                if let Some((purpose, reason)) = retained_diagnostic.take() {
                    let error = ChatError::RetainedHarnessDiagnostic { purpose, reason };
                    durable_failure = Some(error.public_failure());
                    emit_turn_event(&tx, &turn_live, Err(error));
                }
            }

            if turn_failed || provider_stopped_early {
                for (id, name) in finish_unresolved_tool_calls(&mut tools_meta, &mut parts) {
                    emit_turn_event(
                        &tx,
                        &turn_live,
                        Ok(ChatStreamEvent::ToolResult {
                            id,
                            name,
                            content: INTERRUPTED_TOOL_RESULT.to_string(),
                        }),
                    );
                }
            }

            // Persist the assistant turn once complete. `content` is the full prose
            // for history replay; `metadata.tools` + `metadata.parts` let the UI
            // replay the tool work and interleaving on reload. Skip an entirely
            // empty turn.
            let response_bytes = content.len();
            let tool_count = tools_meta.len();
            if let Some(session_id) = provider_session_id {
                if let Err(error) = ai_conversations::Entity::update_many()
                    .filter(ai_conversations::Column::Id.eq(conv_id))
                    .col_expr(
                        ai_conversations::Column::CliSessionId,
                        Expr::value(Some(session_id)),
                    )
                    .exec(db.as_ref())
                    .await
                {
                    tracing::warn!(
                        conversation_id = conv_id,
                        %error,
                        "failed to persist harness session id"
                    );
                }
            }
            if should_capture_session_title && context_type == "application" {
                if let Some(title) = provider_session_title
                    .as_deref()
                    .map(clean_title)
                    .filter(|title| !title.is_empty())
                {
                    // Do not overwrite a manual rename made while the first
                    // turn was running: replace only the exact seed title the
                    // conversation had when this request started.
                    let condition = match initial_conversation_title.as_deref() {
                        Some(initial) => Condition::all()
                            .add(ai_conversations::Column::Id.eq(conv_id))
                            .add(ai_conversations::Column::Title.eq(initial.to_string())),
                        None => Condition::all()
                            .add(ai_conversations::Column::Id.eq(conv_id))
                            .add(ai_conversations::Column::Title.is_null()),
                    };
                    match ai_conversations::Entity::update_many()
                        .filter(condition)
                        .col_expr(
                            ai_conversations::Column::Title,
                            Expr::value(Some(title.clone())),
                        )
                        .exec(db.as_ref())
                        .await
                    {
                        Ok(result) if result.rows_affected == 1 => {
                            let _ = turn_live.send(WireEvent {
                                event: "conversation_title".to_string(),
                                data: serde_json::json!({ "title": title }).to_string(),
                            });
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            conversation_id = conv_id,
                            %error,
                            "failed to store harness-provided conversation title"
                        ),
                    }
                }
            }
            if !content.is_empty() || !tools_meta.is_empty() {
                let persisted_message_id = match draft_message_id {
                    Some(message_id) => {
                        match persist_assistant_message_with(
                            db.as_ref(),
                            message_id,
                            &content,
                            &tools_meta,
                            &parts,
                            "",
                            false,
                            interrupted_message.as_ref(),
                        )
                        .await
                        {
                            Ok(()) => Some(message_id),
                            Err(error) => {
                                tracing::error!(
                                    conv_id,
                                    message_id,
                                    %error,
                                    "failed to finalize assistant draft"
                                );
                                None
                            }
                        }
                    }
                    None => {
                        let am = ai_messages::ActiveModel {
                            conversation_id: Set(conv_id),
                            role: Set("assistant".to_string()),
                            content: Set(content.clone()),
                            metadata: Set(assistant_message_metadata_with(
                                &tools_meta,
                                &parts,
                                false,
                                interrupted_message.as_ref(),
                            )),
                            created_at: Set(Utc::now()),
                            ..Default::default()
                        };
                        am.insert(db.as_ref()).await.ok().map(|message| message.id)
                    }
                };
                if let Some(message_id) = persisted_message_id {
                    // Best-effort: link any pending actions created during this turn
                    // to the persisted assistant message so the UI can correlate them.
                    let proposed_action_ids =
                        execution_state.lock().await.proposed_action_ids.clone();
                    if !proposed_action_ids.is_empty() {
                        if let Some(pending) = &pending_svc_opt {
                            if let Err(e) =
                                pending.link_message(&proposed_action_ids, message_id).await
                            {
                                tracing::warn!(
                                    conv_id,
                                    "Failed to link pending actions to message {message_id}: {e}"
                                );
                            }
                        }
                    }
                }
            } else if let Some(message_id) = draft_message_id {
                // Preserve the previous behavior for a provider that produced
                // no assistant output at all: the structured failure event is
                // authoritative, not an empty assistant bubble.
                if let Err(error) = ai_messages::Entity::delete_by_id(message_id)
                    .exec(db.as_ref())
                    .await
                {
                    tracing::warn!(
                        conv_id,
                        message_id,
                        %error,
                        "failed to remove empty assistant draft"
                    );
                }
            }
            if let Some(failure) = durable_failure.as_ref() {
                if let Err(error) = persist_conversation_failure(
                    db.as_ref(),
                    conv_id,
                    task_turn_id.as_deref(),
                    failure,
                )
                .await
                {
                    tracing::error!(
                        conversation_id = conv_id,
                        %error,
                        "failed to persist safe AI turn failure"
                    );
                }
            }
            tracing::info!(
            component = "ai_turn_timing",
                turn_id = trace_id,
                conversation_id = conv_id,
                project_id,
                provider = %ai_provider,
                phase = "execution_task_complete",
                total_ms = budget.elapsed().as_millis() as u64,
                response_bytes,
                tool_count,
                failed = turn_failed,
                "AI turn timing"
            );
            turn_failed
        });

        if let Some(turn_id) = active_turn_id.as_ref() {
            let active = ActiveTurn {
                turn_id: turn_id.clone(),
                abort: turn_task.abort_handle(),
                auto_approve_provider_tools: auto_approve_provider_tools.clone(),
                permission_mode: permission_mode.clone(),
            };
            match active_turns.lock() {
                Ok(mut turns) => {
                    turns.insert(conv_id, active);
                }
                Err(poisoned) => {
                    poisoned.into_inner().insert(conv_id, active);
                }
            }
        }

        // The database is the source of truth for turn ownership. A Stop that
        // raced workspace preparation clears this claim even when there was no
        // in-memory abort handle yet; in that case dropping the barrier keeps
        // the newly spawned task from starting after it was cancelled.
        let startup_authorized = match active_turn_id.as_deref() {
            Some(turn_id) => match ai_conversations::Entity::find_by_id(conv_id)
                .one(self.db.as_ref())
                .await
            {
                Ok(Some(current)) => {
                    // A permission change may race the workspace preparation
                    // that precedes active-turn registration. Re-read the
                    // durable value before releasing the startup barrier so
                    // that change still applies to this turn.
                    auto_approve_provider_tools.store(
                        permission_mode_auto_approves_provider_tools(&current.ai_permission_mode),
                        Ordering::Release,
                    );
                    match permission_mode.lock() {
                        Ok(mut mode) => mode.clone_from(&current.ai_permission_mode),
                        Err(poisoned) => poisoned
                            .into_inner()
                            .clone_from(&current.ai_permission_mode),
                    }
                    current.turn_status == "running"
                        && current.active_turn_id.as_deref() == Some(turn_id)
                }
                Ok(None) => false,
                Err(error) => {
                    tracing::error!(
                        conv_id,
                        turn_id,
                        error = %error,
                        "failed to verify AI turn ownership before execution"
                    );
                    emit_turn_event(
                        &startup_event_tx,
                        &startup_live,
                        Err(ChatError::Ai(
                            "The server could not verify this turn before execution. Please try again."
                                .to_string(),
                        )),
                    );
                    false
                }
            },
            // Unit-level tool-loop callers do not claim a durable turn.
            None => true,
        };
        if startup_authorized {
            let _ = startup_sender.send(());
        }

        // A monitor, not the SSE response, owns the task handle. It publishes a
        // terminal event and releases the persisted claim even when no browser
        // is currently attached.
        tokio::spawn(async move {
            let outcome = turn_task.await;
            if let Some(turn_id) = monitor_turn_id {
                match active_turns.lock() {
                    Ok(mut turns) => {
                        if turns
                            .get(&conv_id)
                            .is_some_and(|active| active.turn_id == turn_id)
                        {
                            turns.remove(&conv_id);
                        }
                    }
                    Err(poisoned) => {
                        let mut turns = poisoned.into_inner();
                        if turns
                            .get(&conv_id)
                            .is_some_and(|active| active.turn_id == turn_id)
                        {
                            turns.remove(&conv_id);
                        }
                    }
                }
                let terminal_status = match &outcome {
                    Ok(false) => "completed",
                    Ok(true) | Err(_) => "failed",
                };
                if terminal_status == "failed" {
                    let fallback = crate::PublicChatFailure {
                        code: "turn_failed_without_detail",
                        title: "AI turn failed",
                        detail: "The AI turn stopped before Temps retained a safe failure detail. Retry the message; if it fails again, open session diagnostics.".to_string(),
                        retryable: true,
                    };
                    if let Err(error) = persist_conversation_failure(
                        monitor_db.as_ref(),
                        conv_id,
                        Some(&turn_id),
                        &fallback,
                    )
                    .await
                    {
                        tracing::error!(
                            conversation_id = conv_id,
                            turn_id,
                            %error,
                            "failed to persist fallback AI turn failure"
                        );
                    }
                }
                if let Err(error) = ai_conversations::Entity::update_many()
                    .filter(ai_conversations::Column::Id.eq(conv_id))
                    .filter(ai_conversations::Column::ActiveTurnId.eq(&turn_id))
                    .col_expr(
                        ai_conversations::Column::TurnStatus,
                        Expr::value(terminal_status),
                    )
                    .col_expr(
                        ai_conversations::Column::ActiveTurnId,
                        Expr::value(Option::<String>::None),
                    )
                    .col_expr(
                        ai_conversations::Column::TurnStartedAt,
                        Expr::value(Option::<chrono::DateTime<Utc>>::None),
                    )
                    .exec(monitor_db.as_ref())
                    .await
                {
                    tracing::error!(conv_id, turn_id, "failed to finalize AI turn: {error}");
                }
                if terminal_status == "completed" {
                    if let Some((ai, project_id, provider, model)) = monitor_title {
                        let db = monitor_db.clone();
                        tokio::spawn(async move {
                            refresh_title_from_conversation(
                                &ai, &db, conv_id, project_id, provider, model,
                            )
                            .await;
                        });
                    }
                }
                tracing::info!(
                    component = "ai_turn_timing",
                    turn_id,
                    conversation_id = conv_id,
                    phase = "turn_finalized",
                    terminal_status,
                    total_ms = monitor_started.elapsed().as_millis() as u64,
                    "AI turn timing"
                );
                if outcome.as_ref().is_err_and(|error| error.is_panic()) {
                    let _ = monitor_live.send(WireEvent {
                        event: "error".to_string(),
                        data: "The server-side AI turn stopped unexpectedly.".to_string(),
                    });
                }
            }
            let _ = monitor_live.send(WireEvent {
                event: "turn_complete".to_string(),
                data: String::new(),
            });
        });

        let out = async_stream::stream! {
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Box::pin(out)
    }
}

#[cfg(test)]
mod tests;
