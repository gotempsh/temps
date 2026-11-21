// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::watches::{WatchChange, WatchError, WatchPanel};
use temps_core::ai_tool_call::{AiToolCall, ChatWatchScope};
use temps_plugin_protocol::channel::VerifiedPluginApiCaller;

impl From<WatchError> for Problem {
    fn from(error: WatchError) -> Self {
        let status = match &error {
            WatchError::Rejected { status, .. } => *status,
            WatchError::Store { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        temps_core::problemdetails::new(status)
            .with_title("Plugin watch request failed")
            .with_detail(error.to_string())
    }
}
#[derive(Serialize)]
struct WatchAudit<'a> {
    context: temps_core::audit::AuditContext,
    chat: &'a str,
    change: &'a WatchChange,
}
impl temps_core::audit::AuditOperation for WatchAudit<'_> {
    fn operation_type(&self) -> String {
        "PLUGIN_CHAT_WATCH_CHANGED".into()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}
fn check_scope(
    chat: &str,
    ai: Option<Extension<AiToolCall>>,
    scope: Option<Extension<ChatWatchScope>>,
    plugin: Option<Extension<VerifiedPluginApiCaller>>,
) -> Result<(), Problem> {
    if ai.is_some()
        && scope
            .as_ref()
            .is_none_or(|Extension(scope)| scope.0 != chat)
    {
        return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("An agent can manage watches only through its own conversation"));
    }
    if plugin.is_some() {
        return Err(temps_core::problemdetails::new(StatusCode::FORBIDDEN)
            .with_title("Managing watches requires the chat owner"));
    }
    Ok(())
}

type WatchCaller = (
    Option<Extension<AiToolCall>>,
    Option<Extension<ChatWatchScope>>,
    Option<Extension<VerifiedPluginApiCaller>>,
);

#[utoipa::path(get,tag="External Plugins",path="/ai/conversations/{chat}/plugin-watches",params(("chat"=String,Path)),responses((status=200,body=WatchPanel),(status=401),(status=403),(status=404),(status=500)),security(("bearer_auth"=[])))]
pub(super) async fn list_chat_plugin_watches(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(chat): Path<String>,
    caller: WatchCaller,
) -> Result<Json<WatchPanel>, Problem> {
    permission_guard!(auth, ProjectsRead);
    let (ai, scope, plugin) = caller;
    check_scope(&chat, ai, scope, plugin)?;
    Ok(Json(state.service.watch_panel(&auth, &chat).await?))
}
#[utoipa::path(post,tag="External Plugins",path="/ai/conversations/{chat}/plugin-watches",params(("chat"=String,Path)),request_body=WatchChange,responses((status=204),(status=400),(status=401),(status=403),(status=404),(status=409),(status=500)),security(("bearer_auth"=[])))]
pub(super) async fn change_chat_plugin_watch(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(chat): Path<String>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    caller: WatchCaller,
    Json(change): Json<WatchChange>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ProjectsRead);
    let (ai, scope, plugin) = caller;
    check_scope(&chat, ai, scope, plugin)?;
    state.service.change_watch(&auth, &chat, &change).await?;
    record_audit(
        &state,
        &WatchAudit {
            context: audit_context(&auth, &metadata),
            chat: &chat,
            change: &change,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_and_cross_chat_tool_calls_cannot_manage_or_inspect_watch_permissions() {
        assert!(check_scope("target", Some(Extension(AiToolCall)), None, None).is_err());
        assert!(check_scope(
            "target",
            Some(Extension(AiToolCall)),
            Some(Extension(ChatWatchScope("other".into()))),
            None
        )
        .is_err());
        assert!(check_scope(
            "target",
            None,
            None,
            Some(Extension(VerifiedPluginApiCaller::new("reports")))
        )
        .is_err());
        assert!(check_scope(
            "target",
            Some(Extension(AiToolCall)),
            Some(Extension(ChatWatchScope("target".into()))),
            None
        )
        .is_ok());
        assert!(check_scope("target", None, None, None).is_ok());
    }
}
