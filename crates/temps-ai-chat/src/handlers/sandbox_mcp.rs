// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The MCP transport used by an active application-harness turn.

use super::*;

/// MCP transport used only by an active application-harness turn. It is
/// intentionally outside `RequireAuth`: the sandbox does not receive the
/// user's session or an API key. Instead it presents a random, one-turn
/// capability whose in-process executor already captures that user's scoped
/// `AuthContext`.
pub(super) async fn sandbox_tools_mcp(
    State(state): State<Arc<AppState>>,
    Path(bridge_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<temps_ai::mcp::McpRequest>,
) -> Response {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    match state
        .service
        .handle_harness_mcp_request(&bridge_id, bearer, request)
        .await
    {
        Ok(Some(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(HarnessMcpError::NotFound | HarnessMcpError::Unauthorized) => {
            // Do not reveal whether a random bridge id exists.
            (
                StatusCode::UNAUTHORIZED,
                Json(temps_ai::mcp::McpTransportError {
                    error: "sandbox tool capability is not authorized",
                }),
            )
                .into_response()
        }
        Err(HarnessMcpError::Expired) => (
            StatusCode::UNAUTHORIZED,
            Json(temps_ai::mcp::McpTransportError {
                error: "sandbox tool capability has expired",
            }),
        )
            .into_response(),
    }
}
