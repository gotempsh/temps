// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Same-conversation watch management. The model can supply a filter, never
//! the destination conversation, caller, internal route or HTTP method.

use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use temps_ai::streaming::ChatTool;
use temps_auth::context::AuthContext;
use temps_core::{
    ai_tool_call::{AiToolCall, ChatWatchScope},
    RequestMetadata,
};
use tower::ServiceExt;

pub(crate) const TOOL: &str = "temps_watch";

#[derive(Clone, Default)]
pub struct WatchTools(Arc<OnceLock<Router>>);

impl WatchTools {
    pub fn connect(&self, router: Router) {
        let _ = self.0.set(router);
    }
    pub(crate) fn ready(&self) -> bool {
        self.0.get().is_some()
    }
}

pub(crate) struct ConversationWatches {
    pub tools: WatchTools,
    pub chat: String,
    pub metadata: RequestMetadata,
}

impl ConversationWatches {
    pub async fn dispatch(&self, auth: &AuthContext, arguments: &str) -> String {
        let Ok(mut input) = serde_json::from_str::<serde_json::Map<String, Value>>(arguments)
        else {
            return "Invalid watch arguments. Supply a JSON object.".into();
        };
        let Some(router) = self.tools.0.get() else {
            return "Watch service unavailable".into();
        };
        let read = match input.get("action").and_then(Value::as_str) {
            Some("discover" | "list") => true,
            Some("create") => {
                input.entry("mode").or_insert_with(|| json!("wake"));
                false
            }
            Some("pause" | "resume" | "remove") => false,
            _ => return "Unknown watch action".into(),
        };
        let input = Value::Object(input);
        let path = format!("/api/ai/conversations/{}/plugin-watches", self.chat);
        let Ok(mut request) = Request::builder()
            .uri(path)
            .method(if read { "GET" } else { "POST" })
            .header("content-type", "application/json")
            .body(if read {
                Body::empty()
            } else {
                Body::from(input.to_string())
            })
        else {
            return "Invalid conversation reference".into();
        };
        request.extensions_mut().insert(auth.clone());
        request.extensions_mut().insert(self.metadata.clone());
        request.extensions_mut().insert(AiToolCall);
        request
            .extensions_mut()
            .insert(ChatWatchScope(self.chat.clone()));

        let response = match router.clone().oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status();
        match to_bytes(response.into_body(), 256 * 1024).await {
            Ok(body) if status.is_success() && body.is_empty() => {
                json!({"ok":true,"action":input["action"]}).to_string()
            }
            Ok(body) if status.is_success() => String::from_utf8_lossy(&body).into_owned(),
            Ok(body) => format!(
                "Watch request returned {}. {}",
                status,
                String::from_utf8_lossy(&body)
            ),
            Err(_) => "Watch response exceeded the size limit".into(),
        }
    }
}

pub(crate) fn tool() -> ChatTool {
    ChatTool {
        name: TOOL.into(),
        description: "You manage persistent watches for the user in this conversation. When asked to watch a named resource, discover the plugin event source, digest and filter fields, then use authorized plugin read tools to resolve its name to the required IDs. Ask for clarification if the resource is ambiguous. Create the watch yourself using plugin, source, digest and where. By default a matching event wakes this chat to follow the user’s watch request. Choose notify only for a notification without an agent turn. List watches to find IDs for pause, resume or remove.".into(),
        parameters: json!({"type":"object","additionalProperties":false,"required":["action"],
            "properties":{
                "action":{"type":"string","enum":["discover","create","list","pause","resume","remove"]},
                "plugin":{"type":"string"}, "source":{"type":"string"}, "digest":{"type":"string"},
                "where":{"type":"object","description":"Exact matches on the source's declared filterable fields using their declared JSON types."},
                "mode":{"type":"string","enum":["notify","wake"],"default":"wake"}, "id":{"type":"string"}
            }}),
    }
}
