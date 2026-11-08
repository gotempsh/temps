// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::ai_tools::{PluginAiToolsResponse, UpdatePluginAiTools};

#[derive(Serialize)]
struct AiToolsConsentAudit {
    context: temps_core::audit::AuditContext,
    plugin: String,
    actor_id: String,
    enabled: bool,
}
impl temps_core::audit::AuditOperation for AiToolsConsentAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGIN_AI_TOOLS_CHANGED".into()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[utoipa::path(tag="External Plugins", get, path="/x/plugins/{name}/ai-tools", params(("name"=String,Path)), responses((status=200,body=PluginAiToolsResponse),(status=401),(status=403),(status=404),(status=500)), security(("bearer_auth"=[])))]
pub(super) async fn get_plugin_ai_tools(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Path(name): Path<String>,
) -> Result<Json<PluginAiToolsResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    state
        .service
        .plugin_ai_tools(&name)
        .await
        .map(Json)
        .map_err(grant_problem)
}

#[utoipa::path(tag="External Plugins", put, path="/x/plugins/{name}/ai-tools", params(("name"=String,Path)), request_body=UpdatePluginAiTools, responses((status=200,body=PluginAiToolsResponse),(status=401),(status=403),(status=404),(status=409),(status=500)), security(("bearer_auth"=[])))]
pub(super) async fn put_plugin_ai_tools(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(name): Path<String>,
    Json(request): Json<UpdatePluginAiTools>,
) -> Result<Json<PluginAiToolsResponse>, Problem> {
    permission_guard!(auth, SystemAdmin);
    temps_auth::require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        temps_core::SensitiveAction::ChangeExternalPluginGrants { name: name.clone() },
    )
    .await?;
    let event = AiToolsConsentAudit {
        context: audit_context(&auth, &metadata),
        plugin: name.clone(),
        actor_id: request.actor_id.clone(),
        enabled: request.enabled,
    };
    let response = state
        .service
        .update_plugin_ai_tools(&name, request)
        .await
        .map_err(grant_problem)?;
    record_audit(&state, &event).await;
    Ok(Json(response))
}
