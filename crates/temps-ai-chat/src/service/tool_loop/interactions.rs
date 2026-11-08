// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The executors a turn hands to its provider and its MCP bridge: permission
//! prompts, conversation tools and managed processes. Each one re-checks the
//! initiating credential when it runs, not when the turn started.

use super::*;

pub(super) type PermissionRegistrar = Arc<
    dyn Fn(
            temps_ai::streaming::PermissionRequest,
            PendingPermissionOrigin,
        ) -> BoxFuture<'static, Result<PermissionResolution, temps_ai::AiError>>
        + Send
        + Sync,
>;

/// Re-checks the credential that started a turn. Cloned into each executor.
#[derive(Clone)]
pub(super) struct TurnAuthorization {
    pub(super) db: Arc<DatabaseConnection>,
    pub(super) auth: AuthContext,
    pub(super) project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    pub(super) application_id: Option<i64>,
    pub(super) harness_project_ids: Vec<i32>,
    pub(super) provider: String,
    pub(super) permission_mode: Arc<Mutex<String>>,
    pub(super) uses_harness: bool,
}

impl TurnAuthorization {
    pub(super) fn active_mode(&self) -> String {
        active_permission_mode(&self.permission_mode)
    }

    /// Refresh against the harness requirements, in the mode active right now.
    pub(super) async fn refresh_harness(
        &self,
    ) -> Result<AuthContext, ToolAuthorizationRefreshError> {
        let mode = self.active_mode();
        self.refresh_harness_in_mode(&mode).await
    }

    async fn refresh_harness_in_mode(
        &self,
        permission_mode: &str,
    ) -> Result<AuthContext, ToolAuthorizationRefreshError> {
        refresh_harness_authorization(
            self.db.as_ref(),
            &self.auth,
            self.project_access_checker.as_ref(),
            self.application_id,
            &self.harness_project_ids,
            &self.provider,
            permission_mode,
        )
        .await
    }

    pub(super) async fn refresh_tool(&self) -> Result<AuthContext, ToolAuthorizationRefreshError> {
        refresh_tool_authorization(self.db.as_ref(), &self.auth).await
    }
}

/// Install pending permissions in the conversation's registry. Registration
/// happens when this is called, not when the returned future is first polled,
/// so a waiter always exists before its card can become visible.
pub(super) fn permission_registrar(
    conv_public_id: String,
    registry: Arc<Mutex<HashMap<String, PendingPermissionEntry>>>,
) -> PermissionRegistrar {
    Arc::new(move |request, origin| {
        let conv_public_id = conv_public_id.clone();
        let registry = registry.clone();
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
                    reason: format!("permission request '{}' is already pending", request.id),
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
    })
}

/// Approve a provider tool automatically, unless the credential has lapsed. The
/// mode is read now, before the future is first polled.
fn auto_approve_provider_tool(
    authorization: TurnAuthorization,
) -> BoxFuture<'static, Result<PermissionDecision, temps_ai::AiError>> {
    let permission_mode = authorization.active_mode();
    Box::pin(async move {
        let refreshed = if authorization.uses_harness {
            authorization
                .refresh_harness_in_mode(&permission_mode)
                .await
        } else {
            authorization.refresh_tool().await
        };
        refreshed.map_err(|error| {
            tracing::warn!(%error, "denied native tool auto-approval after authorization refresh failed");
            lapsed_credential("chat.permission.authorization")
        })?;
        Ok(PermissionDecision::AllowTool)
    })
}

fn lapsed_credential(purpose: &str) -> temps_ai::AiError {
    temps_ai::AiError::Provider {
        purpose: purpose.to_string(),
        reason: "the initiating credential is no longer authorized".to_string(),
    }
}

/// The interaction bridge handed to provider adapters. They surface their own
/// prompts, so this only registers and waits; it emits nothing.
pub(super) fn provider_interactions(
    registrar: PermissionRegistrar,
    auto_approve: Arc<AtomicBool>,
    authorization: TurnAuthorization,
) -> temps_ai::InteractionExecutor {
    Arc::new(move |request| {
        if auto_approve.load(Ordering::Acquire) && request.kind == PermissionKind::ToolApproval {
            return auto_approve_provider_tool(authorization.clone());
        }
        let request_kind = request.kind.clone();
        let pending = registrar(request, PendingPermissionOrigin::Provider);
        // The mode may have switched to Auto between the first policy check and
        // registration. Re-check after the waiter exists: either the mode-change
        // path already claimed and resolved it, or dropping this exact-generation
        // future removes it before returning the automatic decision. This closes
        // the gap where a newly registered request could otherwise be stranded.
        if auto_approve.load(Ordering::Acquire) && request_kind == PermissionKind::ToolApproval {
            drop(pending);
            return auto_approve_provider_tool(authorization.clone());
        }
        Box::pin(async move { Ok(pending.await?.decision) })
    })
}

/// The interaction bridge for tools a sandbox harness calls over MCP. Nothing
/// else will show these prompts, so this publishes the card itself.
pub(super) fn sandbox_interactions(
    provider_interactions: temps_ai::InteractionExecutor,
    auto_approve: Arc<AtomicBool>,
    events: TurnEvents,
) -> temps_ai::InteractionExecutor {
    Arc::new(move |request| {
        // Calling the registry closure synchronously installs the waiter before
        // the event becomes visible, preventing a fast approval click from racing
        // a missing permission id. The underlying interaction performs the
        // authorization refresh even when Auto mode will approve without showing
        // a card.
        let pending = provider_interactions(request.clone());
        // `provider_interactions` performs its own post-registration policy
        // check. Mirror it here so an approval that was automatically consumed
        // during that race is never rendered as a ghost card.
        if auto_approve.load(Ordering::Acquire) && request.kind == PermissionKind::ToolApproval {
            return pending;
        }
        let safe_input = redact_value(&request.input);
        events.emit(Ok(ChatStreamEvent::PermissionRequested {
            id: request.id,
            kind: request.kind,
            tool_name: request.tool_name,
            input: safe_input,
        }));
        pending
    })
}

/// What the platform interaction bridge needs from its turn.
pub(super) struct PlatformInteractionContext {
    pub(super) registrar: PermissionRegistrar,
    pub(super) auto_approve: Arc<AtomicBool>,
    pub(super) events: TurnEvents,
    pub(super) auth: AuthContext,
    pub(super) metadata: RequestMetadata,
    pub(super) db: Arc<DatabaseConnection>,
    /// The autofix incident this chat belongs to (ADR-054), if any.
    pub(super) autofix_incident: Option<String>,
}

fn auto_resolve_platform_request(
    decision: PermissionDecision,
    auth: AuthContext,
    metadata: RequestMetadata,
    db: Arc<DatabaseConnection>,
) -> BoxFuture<'static, Result<PermissionResolution, temps_ai::AiError>> {
    Box::pin(async move {
        let auth = refresh_tool_authorization(db.as_ref(), &auth)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "denied platform auto-approval after authorization refresh failed");
                lapsed_credential("chat.permission.authorization")
            })?;
        Ok(PermissionResolution {
            decision,
            auth,
            metadata,
        })
    })
}

/// The interaction bridge behind platform tools: writes and questions.
pub(super) fn platform_interactions(
    context: PlatformInteractionContext,
) -> PlatformInteractionExecutor {
    let PlatformInteractionContext {
        registrar,
        auto_approve,
        events,
        auth,
        metadata,
        db,
        autofix_incident,
    } = context;
    Arc::new(move |request| {
        if let Some(incident_public_id) = autofix_incident.clone() {
            let auth = auth.clone();
            let metadata = metadata.clone();
            let db = db.clone();
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
        if auto_approve.load(Ordering::Acquire) {
            if let Some(decision) = automatic_platform_decision(&request) {
                return auto_resolve_platform_request(
                    decision,
                    auth.clone(),
                    metadata.clone(),
                    db.clone(),
                );
            }
        }
        // Registration happens synchronously before the event is published, so
        // a fast click can never race a missing waiter.
        let pending = registrar(request.clone(), PendingPermissionOrigin::PlatformWrite);
        // Permission mode can change while the request is being registered. The
        // active-mode update may already have claimed it; otherwise dropping this
        // exact-generation waiter removes it before the automatic resolution is
        // returned.
        if auto_approve.load(Ordering::Acquire) {
            if let Some(decision) = automatic_platform_decision(&request) {
                drop(pending);
                return auto_resolve_platform_request(
                    decision,
                    auth.clone(),
                    metadata.clone(),
                    db.clone(),
                );
            }
        }
        let safe_input = redact_value(&request.input);
        events.emit(Ok(ChatStreamEvent::PermissionRequested {
            id: request.id,
            kind: request.kind,
            tool_name: request.tool_name,
            input: safe_input,
        }));
        pending
    })
}

/// Routes a tool call to the provider, platform or sandbox that owns it.
/// `project_id` is always the conversation's project, never anything the model
/// supplied, so a tool can't be steered to another tenant's data.
#[derive(Clone)]
pub(super) struct ToolRouter {
    pub(super) project_id: Option<i32>,
    pub(super) conversation_id: i64,
    pub(super) context_id: String,
    pub(super) project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    pub(super) provider: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) api_tools: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) repo_tools: Option<Arc<dyn ConversationContextProvider>>,
    pub(super) write_handle: Option<Arc<temps_ai_api_tools::WriteApiToolsHandle>>,
    pub(super) pending: Option<Arc<PendingActionService>>,
    pub(super) audit: Option<Arc<dyn AuditLogger>>,
    pub(super) interactions: PlatformInteractionExecutor,
    pub(super) state: Arc<tokio::sync::Mutex<ToolExecutionState>>,
}

impl ToolRouter {
    pub(super) async fn dispatch(&self, call: &ToolCall, auth: &AuthContext) -> String {
        let mut state = self.state.lock().await;
        let write_caller = self.write_handle.as_ref().and_then(|handle| handle.get());
        let revision = (
            self.api_tools
                .as_ref()
                .and_then(|provider| provider.api_tools_revision()),
            write_caller
                .as_ref()
                .and_then(|caller| caller.plugin_tools_revision()),
        );
        if state.api_tools_revision != revision {
            // Discovery and refusals from an old publication cannot prevent a
            // fresh proposal. Execution still requires its normal approval.
            state.seen_calls.retain(|key, _| {
                !key.starts_with("temps|")
                    && !key.starts_with("mcp__temps-chat__temps|")
                    && !key.starts_with("temps_write|")
                    && !key.starts_with("mcp__temps-chat__temps_write|")
            });
            state.api_tools_revision = revision;
        }
        dispatch_conversation_tool(
            call,
            self.project_id,
            self.conversation_id,
            &self.context_id,
            auth,
            self.project_access_checker.as_ref(),
            self.provider.as_ref(),
            self.api_tools.as_ref(),
            self.repo_tools.as_ref(),
            write_caller.as_deref(),
            self.pending.as_deref(),
            self.audit.as_deref(),
            Some(&self.interactions),
            &mut state,
        )
        .await
    }
}

/// The executor a provider or MCP bridge calls to run a conversation tool.
/// Incident refusals happen inside `dispatch_conversation_tool` (via
/// `ToolExecutionState::incident`), shared with the gateway rounds loop.
pub(super) fn turn_tool_executor(
    router: ToolRouter,
    authorization: TurnAuthorization,
) -> ToolExecutor {
    Arc::new(move |call: ToolCall| {
        let router = router.clone();
        let authorization = authorization.clone();
        Box::pin(async move {
            let auth = match authorization.refresh_tool().await {
                Ok(auth) => auth,
                Err(error) => {
                    tracing::warn!(
                        conversation_id = router.conversation_id,
                        tool = %call.name,
                        %error,
                        "denied conversation tool after authorization refresh failed"
                    );
                    return Err(lapsed_credential("chat.tool.authorization"));
                }
            };
            Ok(router.dispatch(&call, &auth).await)
        })
    })
}

/// The executor behind the managed-process MCP tools.
pub(super) fn managed_process_executor(
    ai: Arc<dyn AiService>,
    workspace: temps_ai::HarnessWorkspace,
    authorization: TurnAuthorization,
    interactions: temps_ai::InteractionExecutor,
    redaction_values: Vec<String>,
) -> ManagedProcessExecutor {
    let principal_id = authorization.auth.user_id();
    Arc::new(
        move |call: ToolCall| -> BoxFuture<'static, Result<String, temps_ai::AiError>> {
            let ai = ai.clone();
            let workspace = workspace.clone();
            let authorization = authorization.clone();
            let interactions = interactions.clone();
            let redaction_values = redaction_values.clone();
            Box::pin(async move {
                let operation = parse_runtime_process_operation(&call)?;
                authorization.refresh_harness().await.map_err(|error| {
                    tracing::warn!(%error, tool = %call.name, "denied managed process operation after authorization refresh failed");
                    lapsed_credential("chat.runtime_process.authorization")
                })?;

                if matches!(
                    operation,
                    RuntimeProcessOperation::Start { .. }
                        | RuntimeProcessOperation::Stop { .. }
                        | RuntimeProcessOperation::Restart { .. }
                ) {
                    let permission_input: serde_json::Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| {
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
                            reason: "the managed process operation was not approved".to_string(),
                        });
                    }
                    authorization.refresh_harness().await.map_err(|error| {
                        tracing::warn!(%error, tool = %call.name, "denied approved managed process operation after authorization changed");
                        lapsed_credential("chat.runtime_process.authorization")
                    })?;
                }

                let response = tokio::time::timeout(
                    Duration::from_secs(30),
                    ai.runtime_process(RuntimeProcessRequest {
                        principal_id,
                        provider: authorization.provider.clone(),
                        harness_workspace: workspace,
                        operation,
                    }),
                )
                .await
                .map_err(|_| temps_ai::AiError::Provider {
                    purpose: "chat.runtime_process".to_string(),
                    reason: "the managed process operation timed out".to_string(),
                })?
                .map_err(|error| sanitize_runtime_process_error(error, &redaction_values))?;
                serialize_process_result(&response, &redaction_values)
            })
        },
    ) as ManagedProcessExecutor
}
