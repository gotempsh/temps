// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sending messages and streaming a conversation's live events.

use super::*;

/// Live wire for a conversation — cross-tab sync (not represented in the
/// OpenAPI schema; WS upgrades aren't expressible there). Read-only: a second
/// tab watching the same conversation subscribes here to the same authoritative
/// tokens/tool-calls/permission-requests as the sending tab. HTTP message
/// submission never carries model output.
///
/// Auth works identically to any other route: the WS upgrade request is
/// still a normal authenticated HTTP GET (cookies attached) before the 101
/// Switching Protocols — `RequireAuth` reads `AuthContext` out of request
/// extensions populated by the same middleware that runs for `get_conversation`.
pub async fn conversation_stream(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path((project_id, public_id)): Path<(i32, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, Problem> {
    validate_websocket_origin(&headers)?;
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    ensure_conversation_read_permission(&auth, &conv)?;

    // Subscribe before re-reading the snapshot. If a turn completes between
    // the two operations, either the refreshed row is terminal or the already
    // subscribed receiver owns the terminal event; there is no missed-event
    // window that can leave a refreshed tab thinking forever.
    let rx = state.service.subscribe_conversation(conv.id);
    let snapshot = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    let turn_status = snapshot.turn_status;
    let active_turn_id = snapshot.active_turn_id;
    let turn_started_at = snapshot
        .turn_started_at
        .map(|started_at| started_at.to_rfc3339());
    Ok(ws.on_upgrade(move |socket| {
        forward_conversation_events(socket, rx, turn_status, active_turn_id, turn_started_at)
    }))
}

pub async fn user_conversation_stream(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, Problem> {
    validate_websocket_origin(&headers)?;
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    let conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    // A live stream only observes server-owned conversation state. Keep it
    // available to readers so reconnecting a tab cannot hide a running turn
    // merely because that user may not start new sandbox work.
    ensure_user_conversation_access(&state, &auth, &conversation).await?;
    let rx = state.service.subscribe_conversation(conversation.id);
    let snapshot = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    let turn_started_at = snapshot
        .turn_started_at
        .map(|started_at| started_at.to_rfc3339());
    Ok(ws.on_upgrade(move |socket| {
        forward_conversation_events(
            socket,
            rx,
            snapshot.turn_status,
            snapshot.active_turn_id,
            turn_started_at,
        )
    }))
}

/// Forward one conversation's broadcast onto a WebSocket as JSON text frames
/// (`{"event":"...","data":"..."}`) until the client disconnects. On
/// `Lagged` (the bounded channel overflowed — a fast burst while the tab was
/// backgrounded), sends one explicit `resync_required` frame rather than
/// silently resuming with a gap: the client refetches full history instead
/// of rendering a conversation with missing turns.
async fn forward_conversation_events(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<crate::service::WireEvent>,
    turn_status: String,
    active_turn_id: Option<String>,
    turn_started_at: Option<String>,
) {
    let snapshot = turn_state_frame(turn_status, active_turn_id, turn_started_at);
    if socket.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            // A client-initiated close (or any incoming frame — this channel
            // is publish-only, so any message just confirms liveness) ends
            // the loop; ping/pong is handled by axum automatically.
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(evt) => {
                        let frame = serde_json::json!({ "event": evt.event, "data": evt.data });
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let frame = serde_json::json!({ "event": "resync_required", "data": "" });
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

pub(super) fn turn_state_frame(
    turn_status: String,
    active_turn_id: Option<String>,
    turn_started_at: Option<String>,
) -> String {
    serde_json::json!({
        "event": "turn_state",
        "data": serde_json::json!({
            "status": turn_status,
            "turn_id": active_turn_id,
            "turn_started_at": turn_started_at,
        })
        .to_string(),
    })
    .to_string()
}

/// Submit a user message and start a server-owned turn. The command returns as
/// soon as the turn is durable; subscribe to the conversation WebSocket for
/// real-time output.
#[utoipa::path(
    post, tag = "AI Chat",
    path = "/projects/{project_id}/ai/conversations/{public_id}/messages",
    operation_id = "send_project_ai_message",
    params(("project_id" = i32, Path,), ("public_id" = String, Path,)),
    request_body = SendMessageRequest,
    responses((status = 202, description = "Turn accepted; output follows on the conversation WebSocket", body = SendMessageAcceptedResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn send_message(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, public_id)): Path<(i32, String)>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageAcceptedResponse>), Problem> {
    let request_started = tokio::time::Instant::now();
    // Sending a message runs an AI turn (mutates state + incurs cost) → write scope.
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    validate_send_message_request(&req)?;
    if !req.attachments.is_empty() {
        return Err(problemdetails::new(StatusCode::CONFLICT)
            .with_title("Attachments Need a Workspace")
            .with_detail("Open this chat in an AI workspace to attach files."));
    }
    let mut conv = state
        .service
        .get_by_public_id(project_id, auth.user_id(), &public_id)
        .await?;
    ensure_conversation_takes_messages(&conv)?;
    ensure_application_conversation_access(&state, &auth, &conv).await?;
    let effective_permission = req
        .ai_permission_mode
        .as_deref()
        .unwrap_or(&conv.ai_permission_mode);
    // A gateway chat may move to another gateway key; the service rejects any
    // other provider change. Gate on the provider the turn will run on.
    let effective_provider = req.ai_provider.as_deref().unwrap_or(&conv.ai_provider);
    ensure_runtime_permission(&auth, Some(effective_provider), Some(effective_permission))?;
    ensure_enabled(&state, Some(effective_provider)).await?;
    ensure_context_read_permission(&auth, &conv.context_type)?;
    conv = state
        .service
        .update_runtime_selection(
            &conv,
            req.ai_provider.as_deref(),
            req.ai_model.as_deref(),
            req.ai_thinking_level.as_deref(),
            req.ai_permission_mode.as_deref(),
        )
        .await?;
    // Page context is advisory framing, not user content: cap it and silently
    // drop an oversized value rather than failing the message.
    let page_context = req
        .page_context
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_PAGE_CONTEXT_LEN);
    let turn_id = req
        .turn_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let turn_started_at = state.service.claim_turn(&conv, &turn_id).await?;

    // `send_message` persists the user turn before returning the stream, so the
    // turn is durable by the time we audit it. Release the claim if preparation
    // fails before the server-owned task has started.
    match state
        .service
        .send_message(
            &conv,
            &turn_id,
            &req.content,
            None,
            None,
            Vec::new(),
            Vec::new(),
            page_context,
            &auth,
            &metadata,
            state.project_access_checker.clone(),
        )
        .await
    {
        Ok(()) => {}
        Err(error) => {
            if let Err(finish_error) = state
                .service
                .finish_failed_turn(conv.id, &turn_id, &error)
                .await
            {
                error!(
                    conversation_id = conv.id,
                    turn_id, "failed to release AI turn after preparation error: {finish_error}"
                );
            }
            return Err(error.into());
        }
    }
    let execution_enqueued_at = tokio::time::Instant::now();
    info!(
            component = "ai_turn_timing",
        turn_id,
        conversation_id = conv.id,
        project_id,
        provider = %conv.ai_provider,
        phase = "execution_enqueued",
        total_ms = request_started.elapsed().as_millis() as u64,
        "AI turn timing"
    );
    state
        .audit(&ChatMessageSentAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: Some(project_id),
            conversation_id: conv.public_id.clone(),
        })
        .await;
    info!(
            component = "ai_turn_timing",
        turn_id,
        conversation_id = conv.id,
        project_id,
        provider = %conv.ai_provider,
        phase = "http_accepted",
        phase_ms = execution_enqueued_at.elapsed().as_millis() as u64,
        total_ms = request_started.elapsed().as_millis() as u64,
        "AI turn timing"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageAcceptedResponse {
            turn_id,
            status: "running".to_string(),
            turn_started_at: turn_started_at.to_rfc3339(),
        }),
    ))
}

/// Submit a turn to a user-owned conversation. The authenticated user's
/// current role and permissions are captured for this turn's tool executor;
/// no project id from the browser is trusted or required.
#[utoipa::path(
    post, tag = "AI Chat", path = "/ai/conversations/{public_id}/messages",
    params(("public_id" = String, Path,)), request_body = SendMessageRequest,
    responses((status = 202, body = SendMessageAcceptedResponse), (status = 401), (status = 403), (status = 404), (status = 409)),
    security(("bearer_auth" = []))
)]
pub async fn send_user_message(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(public_id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageAcceptedResponse>), Problem> {
    let request_started = tokio::time::Instant::now();
    permission_guard!(auth, ProjectsRead);
    deny_deployment_token!(auth);
    validate_send_message_request(&req)?;

    let mut conversation = state
        .service
        .get_owned_by_public_id(auth.user_id(), &public_id)
        .await?;
    ensure_conversation_takes_messages(&conversation)?;
    ensure_user_conversation_mutation_access(&state, &auth, &conversation).await?;
    if conversation_context_requires_sandbox_write(&conversation.context_type) {
        // Starting an application/global turn grants the provider access to a
        // persistent sandbox. Require the caller's current permission on every
        // turn; conversation ownership alone is not execution authority.
        permission_guard!(auth, SandboxesWrite);
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::SandboxesWrite,
        )
        .await?;
        ensure_application_conversation_permission(
            &state,
            &auth,
            &conversation,
            &Permission::ProjectsWrite,
        )
        .await?;
    }
    let effective_permission = req
        .ai_permission_mode
        .as_deref()
        .unwrap_or(&conversation.ai_permission_mode);
    // A gateway chat may move to another gateway key; the service rejects any
    // other provider change. Gate on the provider the turn will run on.
    let effective_provider = req
        .ai_provider
        .as_deref()
        .unwrap_or(&conversation.ai_provider);
    ensure_runtime_permission(&auth, Some(effective_provider), Some(effective_permission))?;
    if effective_provider != conversation.ai_provider {
        ensure_enabled(&state, Some(effective_provider)).await?;
    }
    conversation = state
        .service
        .update_runtime_selection(
            &conversation,
            req.ai_provider.as_deref(),
            req.ai_model.as_deref(),
            req.ai_thinking_level.as_deref(),
            req.ai_permission_mode.as_deref(),
        )
        .await?;
    let ResolvedAttachments {
        responses: attachments,
        context: attachment_context,
        sandbox: sandbox_attachments,
        images: message_images,
    } = if conversation.context_type == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE {
        resolve_assistant_attachments(&state, &auth, &conversation, &req.attachments).await?
    } else {
        let attachments =
            resolve_chat_attachments(&state, &auth, &conversation, &req.attachments).await?;
        // A gateway model has no sandbox to read the files from, so it gets
        // the images as image parts as well. Host adapters stage the files
        // and read them natively, so they must not get a second copy.
        let images = if conversation
            .ai_provider
            .starts_with(crate::service::GATEWAY_PROVIDER_PREFIX)
            && attachments.iter().any(|attachment| attachment.is_image)
        {
            let support = state
                .assistant_attachments
                .image_support(&conversation.ai_provider, Some(&conversation.ai_model))
                .await;
            crate::attachments::workspace_chat_images(
                attachments
                    .iter()
                    .map(|attachment| (attachment.name.as_str(), attachment.contents.as_slice())),
                support,
                &conversation.ai_model,
            )?
        } else {
            Vec::new()
        };
        ResolvedAttachments {
            context: attachment_prompt_context(&attachments),
            sandbox: attachments
                .iter()
                .map(|attachment| temps_ai::SandboxAttachment {
                    name: attachment.name.clone(),
                    bytes: Arc::from(attachment.contents.clone()),
                    is_image: attachment.is_image,
                })
                .collect(),
            responses: attachments,
            images,
        }
    };
    let attachment_metadata =
        (!attachments.is_empty()).then(|| serde_json::json!({ "attachments": attachments }));
    let page_context = req
        .page_context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_PAGE_CONTEXT_LEN);
    let turn_id = req
        .turn_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let turn_started_at = state.service.claim_turn(&conversation, &turn_id).await?;
    if let Err(error) = state
        .service
        .send_message(
            &conversation,
            &turn_id,
            &req.content,
            attachment_metadata,
            attachment_context.as_deref(),
            sandbox_attachments,
            message_images,
            page_context,
            &auth,
            &metadata,
            state.project_access_checker.clone(),
        )
        .await
    {
        if let Err(finish_error) = state
            .service
            .finish_failed_turn(conversation.id, &turn_id, &error)
            .await
        {
            error!(
                conversation_id = conversation.id,
                turn_id, "failed to release AI turn after preparation error: {finish_error}"
            );
        }
        return Err(error.into());
    }
    if conversation.context_type == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE {
        let ids: Vec<String> = req.attachments.iter().map(|a| a.id.clone()).collect();
        state
            .assistant_attachments
            .mark_sent(conversation.id, &ids)
            .await;
    }
    info!(
        component = "ai_turn_timing",
        turn_id,
        conversation_id = conversation.id,
        project_id = conversation.project_id,
        provider = %conversation.ai_provider,
        phase = "execution_enqueued",
        total_ms = request_started.elapsed().as_millis() as u64,
        "AI turn timing"
    );
    state
        .audit(&ChatMessageSentAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            project_id: conversation.project_id,
            conversation_id: conversation.public_id.clone(),
        })
        .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageAcceptedResponse {
            turn_id,
            status: "running".to_string(),
            turn_started_at: turn_started_at.to_rfc3339(),
        }),
    ))
}
