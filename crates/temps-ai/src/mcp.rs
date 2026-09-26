// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed JSON-RPC envelope for turn-scoped MCP bridges.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpRequestId {
    String(String),
    Number(serde_json::Number),
    Null,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpRequest {
    // Flattening also requires an object on the wire (serde otherwise accepts
    // positional arrays for structs). Unknown protocol extensions are ignored.
    #[serde(flatten)]
    _extensions: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(default, deserialize_with = "present_request_id")]
    pub id: Option<McpRequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Option<McpRequestParams>,
}

fn present_request_id<'de, D>(deserializer: D) -> Result<Option<McpRequestId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    McpRequestId::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpRequestParams {
    #[serde(flatten)]
    _extensions: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    pub name: Option<String>,
    /// Validated by the selected tool, whose argument schema is dynamic.
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpResponse {
    pub jsonrpc: &'static str,
    pub id: McpRequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<McpResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

impl McpResponse {
    pub fn result(id: McpRequestId, result: McpResult) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: McpRequestId, code: i32, message: &'static str) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(McpError { code, message }),
        }
    }

    pub fn call_text(id: McpRequestId, text: impl Into<String>, is_error: bool) -> Self {
        Self::result(id, McpResult::Call(McpCallResult::text(text, is_error)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpError {
    pub code: i32,
    pub message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpResult {
    Initialize(McpInitializeResult),
    Tools(McpToolsResult),
    Call(McpCallResult),
    Empty(McpEmptyResult),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpInitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: McpCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

impl McpInitializeResult {
    pub fn new(name: &'static str) -> Self {
        Self {
            protocol_version: "2024-11-05",
            capabilities: McpCapabilities {
                tools: McpToolCapabilities {
                    list_changed: false,
                },
            },
            server_info: McpServerInfo { name, version: "1" },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpCapabilities {
    pub tools: McpToolCapabilities,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolCapabilities {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolsResult {
    pub tools: Vec<McpToolDefinition>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    /// Tool schemas are supplied dynamically by the registered tool catalog.
    pub input_schema: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpCallResult {
    pub content: Vec<McpTextContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}
impl McpCallResult {
    pub fn text(text: impl Into<String>, is_error: bool) -> Self {
        Self {
            content: vec![McpTextContent {
                kind: "text",
                text: text.into(),
            }],
            is_error,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpTextContent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct McpEmptyResult {}

#[derive(Debug, Clone, Deserialize)]
pub struct McpNativePermissionArguments {
    pub tool_name: String,
    /// Input belongs to the native harness tool, whose schema is not fixed.
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpTransportError {
    pub error: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpPermissionDecision {
    pub behavior: &'static str,
    #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl McpPermissionDecision {
    pub fn allow(input: serde_json::Value) -> Self {
        Self {
            behavior: "allow",
            updated_input: Some(input),
            message: None,
        }
    }
    pub fn deny(message: impl Into<String>) -> Self {
        Self {
            behavior: "deny",
            updated_input: None,
            message: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_id_is_a_notification_but_explicit_null_is_a_request() {
        let notification: McpRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .expect("notification");
        assert!(notification.id.is_none());
        let null: McpRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#)
                .expect("null ID");
        assert_eq!(null.id, Some(McpRequestId::Null));
    }

    #[test]
    fn request_ids_preserve_string_and_numeric_identity() {
        for raw in [r#""request-1""#, "42", "-1", "1.5", "null"] {
            let id: McpRequestId = serde_json::from_str(raw).expect("valid RPC ID");
            assert_eq!(serde_json::to_string(&id).expect("encoded ID"), raw);
        }
        for raw in ["true", "{}", "[]"] {
            assert!(serde_json::from_str::<McpRequestId>(raw).is_err());
        }
    }

    #[test]
    fn rejects_malformed_envelopes_and_tool_params() {
        for raw in [
            r#"{"id":1}"#,
            r#"{"id":1,"method":3}"#,
            r#"{"id":1,"method":"tools/call","params":[]}"#,
            r#"{"id":1,"method":"tools/call","params":{"name":3}}"#,
        ] {
            assert!(serde_json::from_str::<McpRequest>(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn initialize_params_allow_client_specific_metadata() {
        let request: McpRequest = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#).expect("native initialize");
        assert_eq!(request.method, "initialize");
    }

    #[test]
    fn results_and_errors_keep_the_json_rpc_wire_shape() {
        let success = serde_json::to_value(McpResponse::call_text(
            McpRequestId::String("a".into()),
            "done",
            false,
        ))
        .expect("success");
        assert_eq!(
            success,
            serde_json::json!({"jsonrpc":"2.0","id":"a","result":{"content":[{"type":"text","text":"done"}],"isError":false}})
        );
        let error = serde_json::to_value(McpResponse::error(
            McpRequestId::Null,
            -32601,
            "Method not found",
        ))
        .expect("error");
        assert_eq!(
            error,
            serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"Method not found"}})
        );
    }

    #[test]
    fn native_permissions_have_typed_fields_and_preserve_dynamic_input() {
        let args: McpNativePermissionArguments = serde_json::from_value(
            serde_json::json!({"tool_name":"Write","input":{"path":"demo.txt"}}),
        )
        .expect("native input");
        assert_eq!(args.tool_name, "Write");
        let decision =
            serde_json::to_value(McpPermissionDecision::allow(args.input)).expect("allow");
        assert_eq!(
            decision,
            serde_json::json!({"behavior":"allow","updatedInput":{"path":"demo.txt"}})
        );
        assert!(serde_json::from_str::<McpNativePermissionArguments>("{}").is_err());
    }
}
