// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The managed-process MCP tools: their schemas, input validation, and the
//! redaction of their results and errors.

use super::*;

pub(super) type ManagedProcessExecutor =
    Arc<dyn Fn(ToolCall) -> BoxFuture<'static, Result<String, temps_ai::AiError>> + Send + Sync>;

pub(super) const PROCESS_START_TOOL: &str = "temps_process_start";
pub(super) const PROCESS_STATUS_TOOL: &str = "temps_process_status";
pub(super) const PROCESS_LOGS_TOOL: &str = "temps_process_logs";
pub(super) const PROCESS_STOP_TOOL: &str = "temps_process_stop";
pub(super) const PROCESS_RESTART_TOOL: &str = "temps_process_restart";
const MAX_PROCESS_NAME_BYTES: usize = 64;
const MAX_PROCESS_PROGRAM_BYTES: usize = 256;
pub(super) const MAX_PROCESS_ARGUMENTS: usize = 64;
const MAX_PROCESS_ARGUMENT_BYTES: usize = 4 * 1024;
const MAX_PROCESS_ARGUMENTS_BYTES: usize = 16 * 1024;
const MAX_PROCESS_DIRECTORY_BYTES: usize = 512;
const MAX_PROCESS_ID_BYTES: usize = 128;
pub(super) const MAX_PROCESS_RESULT_BYTES: usize = 128 * 1024;
const MAX_PROCESS_LOG_LIMIT: u16 = 200;

pub(super) fn is_managed_process_tool(name: &str) -> bool {
    matches!(
        name,
        PROCESS_START_TOOL
            | PROCESS_STATUS_TOOL
            | PROCESS_LOGS_TOOL
            | PROCESS_STOP_TOOL
            | PROCESS_RESTART_TOOL
    )
}

pub(super) fn managed_process_display_name(name: &str, authoritative_mcp_event: bool) -> String {
    if authoritative_mcp_event && is_managed_process_tool(name) {
        format!("mcp__temps-chat__{name}")
    } else {
        name.to_string()
    }
}

pub(super) fn is_managed_process_echo(name: &str) -> bool {
    is_managed_process_tool(name)
        || [
            PROCESS_START_TOOL,
            PROCESS_STATUS_TOOL,
            PROCESS_LOGS_TOOL,
            PROCESS_STOP_TOOL,
            PROCESS_RESTART_TOOL,
        ]
        .iter()
        .any(|tool| {
            name == format!("temps-chat_{tool}")
                || name
                    .strip_suffix(tool)
                    .is_some_and(|prefix| prefix.ends_with("__"))
        })
}

pub(super) fn managed_process_call_id(
    bridge_id: &str,
    rpc_id: &temps_ai::mcp::McpRequestId,
) -> Result<String, temps_ai::AiError> {
    if matches!(rpc_id, temps_ai::mcp::McpRequestId::Null) {
        return Err(invalid_process_request(
            "the managed process request id is invalid",
        ));
    }
    let normalized = serde_json::to_string(rpc_id)
        .map_err(|_| invalid_process_request("the managed process request id cannot be encoded"))?;
    if normalized.len() > 256 {
        return Err(invalid_process_request(
            "the managed process request id exceeds safe limits",
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"temps-managed-process-v1\0");
    digest.update(bridge_id.as_bytes());
    digest.update(b"\0");
    digest.update(normalized.as_bytes());
    let digest = digest.finalize();
    Ok(format!("tmcp_{}", hex::encode(&digest[..16])))
}

pub(super) fn managed_process_tools() -> Vec<ChatTool> {
    vec![
        ChatTool {
            name: PROCESS_START_TOOL.to_string(),
            description: "Start a distinctly named managed process in the current application workspace. Transport retries of the same call are idempotent; a different call using the same name is rejected. Reuse the returned process id for later operations. Use this for development servers and other long-running commands, not background shell jobs.".to_string(),
            parameters: serde_json::json!({
                "type": "object", "required": ["name", "program"], "additionalProperties": false,
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": MAX_PROCESS_NAME_BYTES},
                    "program": {"type": "string", "minLength": 1, "maxLength": MAX_PROCESS_PROGRAM_BYTES},
                    "args": {"type": "array", "maxItems": MAX_PROCESS_ARGUMENTS, "items": {"type": "string", "maxLength": MAX_PROCESS_ARGUMENT_BYTES}},
                    "directory": {"type": "string", "maxLength": MAX_PROCESS_DIRECTORY_BYTES, "description": "Workspace-relative directory; defaults to the workspace root."},
                    "restart": {"type": "boolean", "default": false}
                }
            }),
        },
        ChatTool {
            name: PROCESS_STATUS_TOOL.to_string(),
            description: "Read the current status of one managed process in this application workspace.".to_string(),
            parameters: serde_json::json!({"type":"object","required":["process_id"],"additionalProperties":false,"properties":{"process_id":{"type":"string","minLength":1,"maxLength":MAX_PROCESS_ID_BYTES}}}),
        },
        ChatTool {
            name: PROCESS_LOGS_TOOL.to_string(),
            description: "Read a bounded page of logs from one managed process in this application workspace.".to_string(),
            parameters: serde_json::json!({"type":"object","required":["process_id"],"additionalProperties":false,"properties":{"process_id":{"type":"string","minLength":1,"maxLength":MAX_PROCESS_ID_BYTES},"after_sequence":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":MAX_PROCESS_LOG_LIMIT}}}),
        },
        ChatTool {
            name: PROCESS_STOP_TOOL.to_string(),
            description: "Stop one managed process in this application workspace. Repeating the request is safe.".to_string(),
            parameters: serde_json::json!({"type":"object","required":["process_id"],"additionalProperties":false,"properties":{"process_id":{"type":"string","minLength":1,"maxLength":MAX_PROCESS_ID_BYTES}}}),
        },
        ChatTool {
            name: PROCESS_RESTART_TOOL.to_string(),
            description: "Restart one managed process in this application workspace.".to_string(),
            parameters: serde_json::json!({"type":"object","required":["process_id"],"additionalProperties":false,"properties":{"process_id":{"type":"string","minLength":1,"maxLength":MAX_PROCESS_ID_BYTES}}}),
        },
    ]
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessStartInput {
    name: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_process_directory")]
    directory: String,
    #[serde(default)]
    restart: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdInput {
    process_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessLogsInput {
    process_id: String,
    after_sequence: Option<u64>,
    limit: Option<u16>,
}

fn default_process_directory() -> String {
    ".".to_string()
}

pub(super) fn invalid_process_request(detail: &'static str) -> temps_ai::AiError {
    temps_ai::AiError::Provider {
        purpose: "chat.runtime_process.validation".to_string(),
        reason: detail.to_string(),
    }
}

fn bounded_plain_value(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
}

fn validate_process_id(process_id: &str) -> Result<(), temps_ai::AiError> {
    if bounded_plain_value(process_id, MAX_PROCESS_ID_BYTES)
        && process_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(invalid_process_request("the managed process id is invalid"))
    }
}

fn validate_process_directory(directory: &str) -> Result<(), temps_ai::AiError> {
    if directory.is_empty()
        || directory.len() > MAX_PROCESS_DIRECTORY_BYTES
        || directory
            .bytes()
            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
    {
        return Err(invalid_process_request(
            "the managed process directory is invalid",
        ));
    }
    let path = std::path::Path::new(directory);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(invalid_process_request(
            "the managed process directory must stay within the application workspace",
        ));
    }
    Ok(())
}

pub(super) fn parse_runtime_process_operation(
    call: &ToolCall,
) -> Result<RuntimeProcessOperation, temps_ai::AiError> {
    match call.name.as_str() {
        PROCESS_START_TOOL => {
            let input: ProcessStartInput = serde_json::from_str(&call.arguments).map_err(|_| {
                invalid_process_request("the managed process start input is invalid")
            })?;
            if !bounded_plain_value(&input.name, MAX_PROCESS_NAME_BYTES) {
                return Err(invalid_process_request(
                    "the managed process name is invalid",
                ));
            }
            if !bounded_plain_value(&input.program, MAX_PROCESS_PROGRAM_BYTES) {
                return Err(invalid_process_request(
                    "the managed process program is invalid",
                ));
            }
            if input.args.len() > MAX_PROCESS_ARGUMENTS
                || input.args.iter().map(String::len).sum::<usize>() > MAX_PROCESS_ARGUMENTS_BYTES
                || input.args.iter().any(|arg| {
                    arg.len() > MAX_PROCESS_ARGUMENT_BYTES
                        || arg.bytes().any(|byte| byte == 0 || byte == b'\r')
                })
            {
                return Err(invalid_process_request(
                    "the managed process arguments exceed safe limits",
                ));
            }
            validate_process_directory(&input.directory)?;
            Ok(RuntimeProcessOperation::Start {
                idempotency_key: call.id.clone(),
                name: input.name,
                program: input.program,
                args: input.args,
                directory: input.directory,
                restart: input.restart,
            })
        }
        PROCESS_STATUS_TOOL => {
            let input: ProcessIdInput = serde_json::from_str(&call.arguments).map_err(|_| {
                invalid_process_request("the managed process status input is invalid")
            })?;
            validate_process_id(&input.process_id)?;
            Ok(RuntimeProcessOperation::Status {
                process_id: input.process_id,
            })
        }
        PROCESS_LOGS_TOOL => {
            let input: ProcessLogsInput = serde_json::from_str(&call.arguments).map_err(|_| {
                invalid_process_request("the managed process logs input is invalid")
            })?;
            validate_process_id(&input.process_id)?;
            if input
                .limit
                .is_some_and(|limit| limit == 0 || limit > MAX_PROCESS_LOG_LIMIT)
            {
                return Err(invalid_process_request(
                    "the managed process log limit is invalid",
                ));
            }
            Ok(RuntimeProcessOperation::Logs {
                process_id: input.process_id,
                after_sequence: input.after_sequence,
                limit: input.limit,
            })
        }
        PROCESS_STOP_TOOL => {
            let input: ProcessIdInput = serde_json::from_str(&call.arguments).map_err(|_| {
                invalid_process_request("the managed process stop input is invalid")
            })?;
            validate_process_id(&input.process_id)?;
            Ok(RuntimeProcessOperation::Stop {
                process_id: input.process_id,
            })
        }
        PROCESS_RESTART_TOOL => {
            let input: ProcessIdInput = serde_json::from_str(&call.arguments).map_err(|_| {
                invalid_process_request("the managed process restart input is invalid")
            })?;
            validate_process_id(&input.process_id)?;
            Ok(RuntimeProcessOperation::Restart {
                process_id: input.process_id,
            })
        }
        _ => Err(invalid_process_request(
            "the managed process operation is not supported",
        )),
    }
}

fn redact_exact_values(value: &mut serde_json::Value, secrets: &[String]) {
    match value {
        serde_json::Value::String(text) => {
            for secret in secrets {
                if !secret.is_empty() && text.contains(secret.as_str()) {
                    *text = text.replace(secret, "***");
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_exact_values(value, secrets);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values_mut() {
                redact_exact_values(value, secrets);
            }
        }
        _ => {}
    }
}

pub(super) fn serialize_process_result(
    result: &temps_ai::RuntimeProcessResponse,
    exact_secrets: &[String],
) -> Result<String, temps_ai::AiError> {
    let value = serde_json::to_value(result).map_err(|_| temps_ai::AiError::Provider {
        purpose: "chat.runtime_process.response".to_string(),
        reason: "the managed process response could not be encoded".to_string(),
    })?;
    let mut safe_value = redact_value(&value);
    redact_exact_values(&mut safe_value, exact_secrets);
    let safe = safe_value.to_string();
    if safe.len() > MAX_PROCESS_RESULT_BYTES {
        return Ok(serde_json::json!({
            "truncated": true,
            "detail": "The managed process response exceeded the chat display limit. Request a smaller log page."
        })
        .to_string());
    }
    Ok(safe)
}

pub(super) fn managed_process_error_text(error: &temps_ai::AiError) -> String {
    let detail = match error {
        temps_ai::AiError::Provider { purpose, reason }
            if purpose.starts_with("chat.application.process") =>
        {
            reason.as_str()
        }
        temps_ai::AiError::Provider { purpose, .. }
            if purpose == "chat.runtime_process.validation" =>
        {
            "The managed process request is invalid. Correct its fields and try again."
        }
        temps_ai::AiError::Provider { purpose, .. }
            if purpose == "chat.runtime_process.authorization" =>
        {
            "Your authorization changed before the managed process operation could run."
        }
        temps_ai::AiError::Provider { purpose, .. }
            if purpose == "chat.runtime_process.permission" =>
        {
            "The managed process operation was not approved."
        }
        temps_ai::AiError::Provider { purpose, .. } if purpose == "chat.runtime_process.busy" => {
            "Another managed process start is in progress for this application. Retry shortly."
        }
        _ => "The managed process operation could not be completed safely.",
    };
    serde_json::json!({"is_error": true, "error": detail}).to_string()
}

pub(super) fn sanitize_runtime_process_error(
    error: temps_ai::AiError,
    exact_secrets: &[String],
) -> temps_ai::AiError {
    match error {
        temps_ai::AiError::Provider { purpose, reason }
            if purpose.starts_with("chat.application.process") =>
        {
            let mut reason = redact_text(&reason);
            for secret in exact_secrets {
                if !secret.is_empty() {
                    reason = reason.replace(secret, "***");
                }
            }
            temps_ai::AiError::Provider {
                purpose,
                reason: reason.chars().take(2_048).collect(),
            }
        }
        other => other,
    }
}
