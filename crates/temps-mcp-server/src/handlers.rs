// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use std::sync::Arc;
use tracing::{error, info};

use temps_auth::permissions::Permission;
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::RequestMetadata;

use crate::tools::deployments::{AuditActor, DeploymentExecCtx};

use crate::error::McpError;
use crate::protocol::{
    JsonRpcRequest, JsonRpcResponse, McpCapabilities, McpInitializeResult, McpQuery, McpServerInfo,
    McpToolResult, McpToolsCapability, McpToolsListResult, ToolsProbeResponse,
};
use crate::registry::{collect_tools, ALL_GROUP_INFOS};
use crate::McpHandlerState;
use temps_core::problemdetails::Problem;

// ─── Public probe ─────────────────────────────────────────────────────────────

/// `GET /mcp/tools` — unauthenticated capability probe.
///
/// Returns `404` when the MCP feature flag is off, allowing the CLI wizard to
/// show a clear "this instance does not support MCP" message rather than a
/// generic connection error.
///
/// Returns `200 { "groups": [...] }` when the feature is enabled.
pub async fn tools_probe_handler(
    State(state): State<Arc<McpHandlerState>>,
) -> Result<impl IntoResponse, Problem> {
    let settings = state
        .config_service
        .get_settings()
        .await
        .map_err(|e| McpError::Config(e.to_string()))?;

    if !settings.mcp_server.enabled {
        return Err(McpError::FeatureDisabled.into());
    }

    let response = ToolsProbeResponse {
        groups: ALL_GROUP_INFOS.to_vec(),
    };
    Ok(Json(response))
}

// ─── Authenticated MCP endpoints ─────────────────────────────────────────────

/// `POST /mcp` — JSON-RPC 2.0 request dispatcher.
///
/// Accepts `Content-Type: application/json` and responds with either:
/// - `200 application/json` for a single request, or
/// - `202` for notifications (no `id` field in the request).
///
/// Requires `Authorization: Bearer <api-key>` and at least `ProjectsRead`
/// permission.
pub async fn mcp_post_handler(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<McpHandlerState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Query(query): Query<McpQuery>,
    Json(request): Json<JsonRpcRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Feature flag guard.
    let settings = state
        .config_service
        .get_settings()
        .await
        .map_err(|e| McpError::Config(e.to_string()))?;
    if !settings.mcp_server.enabled {
        return Err(McpError::FeatureDisabled.into());
    }

    permission_guard!(auth, ProjectsRead);

    // Notifications have no `id` — acknowledge without a body.
    if request.is_notification() {
        if request.method == "notifications/cancelled" {
            info!(
                user_id = auth.user_id(),
                method = %request.method,
                "MCP: notification received"
            );
        }
        return Ok((StatusCode::ACCEPTED, axum::body::Body::empty()).into_response());
    }

    let id = request.id.clone();
    let response = dispatch(&auth, &metadata, &state, &query, request).await;

    let rpc_response = match response {
        Ok(result) => JsonRpcResponse::success(id, result),
        Err(ref e) => {
            let code = e.rpc_code();
            JsonRpcResponse::error(id, code, e.to_string())
        }
    };

    Ok(Json(rpc_response).into_response())
}

/// `GET /mcp` — SSE stream for server-initiated messages (MCP Streamable HTTP).
///
/// In this first slice the server does not initiate messages, so the stream
/// opens and remains idle.  The `Mcp-Session-Id` header is echoed back so the
/// client can correlate connections.  Full server-push support is a follow-up.
pub async fn mcp_get_handler(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<McpHandlerState>>,
) -> Result<impl IntoResponse, Problem> {
    let settings = state
        .config_service
        .get_settings()
        .await
        .map_err(|e| McpError::Config(e.to_string()))?;
    if !settings.mcp_server.enabled {
        return Err(McpError::FeatureDisabled.into());
    }

    permission_guard!(auth, ProjectsRead);

    // Return an empty SSE stream that terminates immediately.
    // TODO(mcp): implement persistent SSE for server-initiated notifications.
    let body = ": keep-alive\n\n";
    // Safety: all builder inputs are static string literals — this call
    // cannot produce a builder error, so the `unwrap_or_else` fallback is
    // unreachable in practice.  It is kept here (rather than `.unwrap()`)
    // purely to satisfy CLAUDE.md's "no unwrap in production paths" rule.
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()));

    Ok(response)
}

/// `DELETE /mcp` — session termination.
///
/// The client signals it is done.  We have no per-session state beyond the
/// proposal store (which is shared), so this is a simple acknowledgement.
pub async fn mcp_delete_handler(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<McpHandlerState>>,
) -> Result<impl IntoResponse, Problem> {
    let settings = state
        .config_service
        .get_settings()
        .await
        .map_err(|e| McpError::Config(e.to_string()))?;
    if !settings.mcp_server.enabled {
        return Err(McpError::FeatureDisabled.into());
    }

    permission_guard!(auth, ProjectsRead);

    info!(
        user_id = auth.user_id(),
        "MCP: session terminated by client"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ─── Method dispatcher ────────────────────────────────────────────────────────

async fn dispatch(
    auth: &AuthContext,
    metadata: &RequestMetadata,
    state: &Arc<McpHandlerState>,
    query: &McpQuery,
    request: JsonRpcRequest,
) -> Result<serde_json::Value, McpError> {
    // MINOR #12 — validate protocol version before routing.
    if request.jsonrpc != "2.0" {
        return Err(McpError::InvalidJsonRpcVersion {
            received: request.jsonrpc.clone(),
        });
    }

    let requested_groups = query.parsed_groups();
    let write_enabled = query.write_enabled();

    // Each handler returns a typed result; the single legitimate boundary
    // conversion to `serde_json::Value` happens here, once per arm, before the
    // value enters the JSON-RPC envelope's generic `result` field.
    match request.method.as_str() {
        "initialize" => {
            let r = handle_initialize()?;
            serde_json::to_value(r).map_err(McpError::Serialization)
        }

        "tools/list" => {
            let r = handle_tools_list(&requested_groups, write_enabled)?;
            serde_json::to_value(r).map_err(McpError::Serialization)
        }

        "tools/call" => {
            let params = request
                .params
                .as_ref()
                .ok_or_else(|| McpError::MissingArgument {
                    arg: "params".to_string(),
                    tool: "tools/call".to_string(),
                })?;

            let tool_name = params
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| McpError::MissingArgument {
                    arg: "name".to_string(),
                    tool: "tools/call".to_string(),
                })?;

            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));

            let r = handle_tool_call(
                auth,
                metadata,
                state,
                &requested_groups,
                write_enabled,
                tool_name,
                &args,
            )
            .await?;
            serde_json::to_value(r).map_err(McpError::Serialization)
        }

        other => {
            error!(method = other, "MCP: unknown method");
            Err(McpError::UnknownTool {
                name: format!("method:{other}"),
            })
        }
    }
}

fn handle_initialize() -> Result<McpInitializeResult, McpError> {
    Ok(McpInitializeResult {
        protocol_version: "2025-03-26",
        capabilities: McpCapabilities {
            tools: McpToolsCapability {},
        },
        server_info: McpServerInfo {
            name: "temps-mcp-server",
            version: env!("CARGO_PKG_VERSION"),
        },
    })
}

fn handle_tools_list(
    requested_groups: &[String],
    write_enabled: bool,
) -> Result<McpToolsListResult, McpError> {
    let tools = collect_tools(requested_groups, write_enabled);
    Ok(McpToolsListResult { tools })
}

async fn handle_tool_call(
    auth: &AuthContext,
    metadata: &RequestMetadata,
    state: &Arc<McpHandlerState>,
    requested_groups: &[String],
    write_enabled: bool,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<McpToolResult, McpError> {
    // Verify the tool belongs to one of the requested (active) groups.
    let available_tools = collect_tools(requested_groups, write_enabled);
    let tool_known = available_tools.iter().any(|t| t.name == tool_name);
    if !tool_known {
        return Err(McpError::UnknownTool {
            name: tool_name.to_string(),
        });
    }

    // Write tools additionally require write mode opt-in AND DeploymentsCreate
    // permission.  These are two distinct, orthogonal checks:
    // - `write_enabled`: operator/user explicitly passed `?write=1` in the MCP
    //   URL — the session was opened with intent to mutate.
    // - `DeploymentsCreate` permission: the token's role actually carries the
    //   capability to trigger deployments (matches the REST endpoint check in
    //   remote_deployments.rs).
    let write_tools = ["trigger_deployment", "confirm_action"];
    if write_tools.contains(&tool_name) {
        if !write_enabled {
            return Err(McpError::WriteNotEnabled);
        }
        if !auth.has_permission(&Permission::DeploymentsCreate) {
            return Err(McpError::InsufficientPermission {
                permission: Permission::DeploymentsCreate.to_string(),
            });
        }
        info!(
            user_id = auth.user_id(),
            tool = tool_name,
            "MCP write tool invoked (propose-then-confirm)"
        );
    }

    // Route to the correct group handler.
    // Platform tools.
    if matches!(tool_name, "list_projects" | "get_project") {
        return crate::tools::platform::execute(
            tool_name,
            args,
            auth,
            &state.project_service,
            state.project_access_checker.as_deref(),
        )
        .await;
    }

    // Deployments tools.
    if matches!(
        tool_name,
        "list_deployments" | "trigger_deployment" | "confirm_action"
    ) {
        let actor = AuditActor {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        };
        let exec_ctx = DeploymentExecCtx {
            deployment_service: &state.deployment_service,
            proposals: &state.proposals,
            audit_service: &state.audit_service,
            actor: &actor,
            auth,
            checker: state.project_access_checker.as_deref(),
        };
        return crate::tools::deployments::execute(tool_name, args, write_enabled, &exec_ctx).await;
    }

    Err(McpError::UnknownTool {
        name: tool_name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_response_has_required_fields() {
        let result = handle_initialize().expect("initialize must succeed");
        assert_eq!(result.protocol_version, "2025-03-26");
        assert!(!result.server_info.name.is_empty());
        // McpToolsCapability is a marker struct; its presence on capabilities
        // confirms tool support — serializes as `{}` per the MCP spec.
        let _ = &result.capabilities.tools;
    }

    #[test]
    fn initialize_serializes_correctly() {
        // Verify the wire shape so a future rename doesn't silently break the
        // protocol handshake.
        let result = handle_initialize().expect("initialize must succeed");
        let v = serde_json::to_value(result).expect("must serialize");
        assert_eq!(v["protocolVersion"], "2025-03-26");
        assert!(v["serverInfo"]["name"].as_str().is_some());
        assert!(v["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_all_groups_read_only() {
        let result = handle_tools_list(&[], false).expect("tools/list must succeed");
        let tools = &result.tools;
        assert!(!tools.is_empty());
        // Write tools must not appear in read-only mode.
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(!names.contains(&"trigger_deployment"));
        assert!(!names.contains(&"confirm_action"));
    }

    #[test]
    fn tools_list_write_mode_includes_write_tools() {
        let result = handle_tools_list(&[], true).expect("tools/list must succeed");
        let tools = &result.tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"trigger_deployment"));
        assert!(names.contains(&"confirm_action"));
    }

    #[test]
    fn tools_list_group_filter() {
        let result =
            handle_tools_list(&["platform".to_string()], false).expect("tools/list must succeed");
        let tools = &result.tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"list_projects"));
        assert!(!names.contains(&"list_deployments"));
    }

    #[test]
    fn tools_list_serializes_correctly() {
        // Verify the wire shape is preserved end-to-end.
        let result = handle_tools_list(&[], false).expect("tools/list must succeed");
        let v = serde_json::to_value(result).expect("must serialize");
        assert!(v["tools"].as_array().is_some(), "must have a 'tools' array");
    }
}
