// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::watches::{WatchChange, WatchError, WatchPanel};
use temps_core::ai_tool_call::{AiToolCall, ChatWatchScope};
use temps_plugin_protocol::channel::VerifiedPluginApiCaller;

impl From<WatchError> for Problem {
    fn from(error: WatchError) -> Self {
        let (status, detail) = match &error {
            WatchError::Rejected { status, .. } => (*status, error.to_string()),
            WatchError::Store { operation, source } => {
                tracing::error!(operation, %source, "Plugin watch storage error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not access plugin watch storage".into(),
                )
            }
        };
        temps_core::problemdetails::new(status)
            .with_title("Plugin watch request failed")
            .with_detail(detail)
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

#[utoipa::path(
    get, tag = "External Plugins", path = "/ai/conversations/{chat}/plugin-watches",
    params(("chat" = String, Path, description = "Conversation ID")),
    responses(
        (status = 200, description = "Watches and event sources for the conversation", body = WatchPanel),
        (status = 400, description = "Invalid conversation ID"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Caller cannot access watches in this conversation"),
        (status = 404, description = "Conversation or watch resource not found"),
        (status = 500, description = "Could not access watch storage"),
        (status = 503, description = "Watch service unavailable"),
        (status = 504, description = "Conversation access check timed out"),
    ),
    security(("bearer_auth" = []))
)]
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
#[utoipa::path(
    post, tag = "External Plugins", path = "/ai/conversations/{chat}/plugin-watches",
    params(("chat" = String, Path, description = "Conversation ID")),
    request_body = WatchChange,
    responses(
        (status = 204, description = "Watch changed"),
        (status = 400, description = "Invalid watch request"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Caller cannot access watches in this conversation"),
        (status = 404, description = "Conversation or watch resource not found"),
        (status = 409, description = "Watch configuration conflict"),
        (status = 500, description = "Could not access watch storage"),
        (status = 502, description = "Invalid plugin access response"),
        (status = 503, description = "Watch service or plugin unavailable"),
        (status = 504, description = "Access check timed out"),
    ),
    security(("bearer_auth" = []))
)]
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
    #[tokio::test]
    async fn watch_storage_errors_keep_diagnostics_out_of_the_response() {
        use axum::response::IntoResponse;
        let response = Problem::from(WatchError::Store {
            operation: "list watches",
            source: sea_orm::DbErr::Custom("private fixture diagnostic".into()),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["detail"], "Could not access plugin watch storage");
    }

    #[test]
    fn watch_api_describes_list_and_change_responses() {
        let spec = serde_json::to_value(ExternalPluginsApiDoc::openapi()).expect("OpenAPI");
        let path = &spec["paths"]["/ai/conversations/{chat}/plugin-watches"];
        for method in ["get", "post"] {
            let responses = path[method]["responses"].as_object().expect("responses");
            for (code, response) in responses {
                assert!(
                    response["description"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty()),
                    "missing {method} {code} description"
                );
            }
        }
    }

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
