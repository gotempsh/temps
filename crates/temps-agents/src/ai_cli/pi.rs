// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The pi coding agent (<https://pi.dev>).
//!
//! pi runs only inside Temps workspaces: the retained sandbox runtime drives
//! `pi --mode rpc` through the agent runtime SDK, and the turn-scoped model
//! relay supplies its provider credential. Host execution is not supported,
//! because a host process would need a reusable provider key and pi cannot
//! take a turn's scoped Temps tools from the command line.

use async_trait::async_trait;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::{
    sanitize_command_environment, AiCliModelCapability, AiCliProvider, AiCliStatus, AiRunConfig,
    AiRunResult,
};
use crate::error::AgentError;

/// Stable provider id stored in settings and conversations.
pub const PROVIDER_ID: &str = "pi";

const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// Response ids used by [`WORKSPACE_MODEL_DISCOVERY_SCRIPT`].
const MODELS_RESPONSE_ID: &str = "temps-models";
const STATE_RESPONSE_ID: &str = "temps-state";

/// Lists the models pi can use with the credentials in its environment, and
/// its default model and thinking level, over pi's RPC protocol. pi answers
/// both commands and exits when its stdin closes; neither contacts a model.
pub const WORKSPACE_MODEL_DISCOVERY_SCRIPT: &str = concat!(
    "printf '%s\\n' ",
    "'{\"type\":\"get_available_models\",\"id\":\"temps-models\"}' ",
    "'{\"type\":\"get_state\",\"id\":\"temps-state\"}' ",
    "| exec pi --mode rpc --no-session --no-approve --no-extensions --no-skills ",
    "--no-prompt-templates --no-context-files"
);

/// Temps' pi agent directory inside a workspace, relative to the sandbox
/// user's home. pi reads model endpoints and MCP servers only from files in
/// its agent directory, so Temps keeps its own instead of the user's
/// `~/.pi/agent`, rewrites both files before every pi operation, and lets
/// pi's sessions persist beside them for resume.
pub const WORKSPACE_AGENT_DIR_REL: &str = ".temps-pi/agent";

/// Environment variable pi expands in the Temps MCP server's headers. The
/// turn's MCP token stays in the process environment and never touches disk.
pub const MCP_AUTHORIZATION_ENV: &str = "TEMPS_CHAT_MCP_AUTHORIZATION";

/// Name of the Temps MCP server; pi exposes its tools as
/// `mcp__temps_chat__<tool>`.
pub const MCP_SERVER_NAME: &str = "temps-chat";

/// Absolute path of [`WORKSPACE_AGENT_DIR_REL`].
pub fn workspace_agent_dir() -> String {
    format!(
        "{}/{WORKSPACE_AGENT_DIR_REL}",
        crate::sandbox::user::SANDBOX_HOME
    )
}

/// Environment variable holding the turn's model relay capability. pi reads
/// it through `models.json`, which takes precedence over provider variables
/// such as `OPENAI_API_KEY`, so an application's own keys in the workspace
/// environment are neither used by pi nor replaced for its shell commands.
pub const MODEL_RELAY_TOKEN_ENV: &str = "TEMPS_PI_MODEL_RELAY_TOKEN";

/// pi's `models.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelsConfig {
    pub providers: BTreeMap<String, ProviderEndpoint>,
}

/// Overrides for one of pi's built-in providers. pi keeps the provider's own
/// model catalog and appends its API path (`/v1/messages`, `/responses`) to
/// `base_url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEndpoint {
    pub base_url: String,
    /// A `$NAME` reference pi resolves from its environment.
    pub api_key: String,
    /// Values may hold `${NAME}` references pi resolves from its environment.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// pi's `mcp.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    pub mcp_servers: BTreeMap<String, McpHttpServer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpHttpServer {
    pub url: String,
    /// Values may hold `${NAME}` references pi resolves from its environment.
    pub headers: BTreeMap<String, String>,
    pub exposure: McpToolExposure,
}

/// How pi offers an MCP server's tools to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpToolExposure {
    /// Every tool is declared to the model, as Temps' other harnesses do.
    Direct,
}

/// `models.json` pointing one built-in pi provider (`anthropic` or `openai`)
/// at the turn's model relay.
pub fn models_config(provider: &str, base_url: &str) -> ModelsConfig {
    let mut headers = BTreeMap::new();
    if provider == "anthropic" {
        // pi sends an Anthropic key as `x-api-key`, which the relay neither
        // accepts nor forwards; the relay reads its capability as a bearer.
        headers.insert(
            "Authorization".to_string(),
            format!("Bearer ${{{MODEL_RELAY_TOKEN_ENV}}}"),
        );
    }
    ModelsConfig {
        providers: BTreeMap::from([(
            provider.to_string(),
            ProviderEndpoint {
                base_url: base_url.to_string(),
                api_key: format!("${MODEL_RELAY_TOKEN_ENV}"),
                headers,
            },
        )]),
    }
}

/// `mcp.json` for one operation. Without a server it is empty, so a stale
/// server from an earlier turn is never contacted.
pub fn mcp_config(server_url: Option<&str>) -> McpConfig {
    McpConfig {
        mcp_servers: server_url
            .map(|url| {
                (
                    MCP_SERVER_NAME.to_string(),
                    McpHttpServer {
                        url: url.to_string(),
                        headers: BTreeMap::from([(
                            "Authorization".to_string(),
                            format!("${{{MCP_AUTHORIZATION_ENV}}}"),
                        )]),
                        exposure: McpToolExposure::Direct,
                    },
                )
            })
            .into_iter()
            .collect(),
    }
}

/// A minimal, tool-less model request used to verify a saved credential.
/// Every source of ambient configuration is off, and `--no-approve` ignores
/// project-local pi files.
pub fn verification_command(model: &str) -> Vec<String> {
    [
        "pi",
        "--print",
        "--mode",
        "json",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-approve",
        "--model",
        model,
        "--",
        "Reply OK.",
    ]
    .map(str::to_string)
    .to_vec()
}

/// pi's thinking levels, lowest first.
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const DEFAULT_THINKING_LEVEL: &str = "medium";
const MAX_DISCOVERED_MODELS: usize = 512;
const MAX_MODEL_ID_CHARS: usize = 200;
const MAX_MODEL_NAME_CHARS: usize = 120;

pub struct PiCliProvider;

fn version_command() -> Command {
    let mut command = Command::new("pi");
    sanitize_command_environment(&mut command);
    // `--version` never needs the network; this also stops pi from checking
    // for a newer release.
    command.env("PI_SKIP_VERSION_CHECK", "1");
    command.kill_on_drop(true);
    command
}

fn host_execution_unsupported() -> AgentError {
    AgentError::AiCliWorkspaceChatOnly {
        provider: PROVIDER_ID.to_string(),
        operation: "host execution".to_string(),
    }
}

#[async_trait]
impl AiCliProvider for PiCliProvider {
    fn name(&self) -> &str {
        PROVIDER_ID
    }

    async fn check_installed(&self) -> bool {
        tokio::time::timeout(
            STATUS_TIMEOUT,
            version_command()
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|status| status.success())
    }

    /// Reports the host binary for information only. pi is never
    /// authenticated on the host: its credential is relayed into a workspace
    /// for one turn at a time.
    async fn get_status(&self) -> AiCliStatus {
        let output = tokio::time::timeout(
            STATUS_TIMEOUT,
            version_command()
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output(),
        )
        .await;
        let version = match output {
            Ok(Ok(output)) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .filter(|version| !version.is_empty())
            }
            _ => None,
        };
        AiCliStatus {
            provider: PROVIDER_ID.into(),
            installed: version.is_some(),
            version,
            authenticated: false,
            auth_method: None,
            email: None,
            subscription_type: None,
            setup_hint: Some(
                "pi runs inside Temps workspaces. Save an Anthropic or OpenAI API key for pi in Agent Sandbox settings; host execution is not supported."
                    .into(),
            ),
        }
    }

    fn extract_assistant_text(&self, line: &str) -> Option<String> {
        extract_assistant_text(line)
    }

    async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        Err(host_execution_unsupported())
    }

    async fn continue_conversation(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        Err(host_execution_unsupported())
    }
}

/// One event of pi's `--mode json` output.
#[derive(Deserialize)]
struct JsonModeEvent {
    #[serde(rename = "type")]
    kind: String,
    message: Option<JsonModeMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonModeMessage {
    role: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    content: Option<MessageContent>,
}

/// Assistant content is a list of blocks; other messages may carry a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Blocks(Vec<ContentBlock>),
    Other(IgnoredAny),
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

/// The text of a completed assistant message in pi's `--mode json` output.
/// A message that ended in an error carries no answer, even if pi exits 0.
pub fn extract_assistant_text(line: &str) -> Option<String> {
    let event = serde_json::from_str::<JsonModeEvent>(line.trim()).ok()?;
    let message = event.message.filter(|message| {
        event.kind == "message_end"
            && message.role == "assistant"
            && !matches!(message.stop_reason.as_deref(), Some("error" | "aborted"))
    })?;
    let Some(MessageContent::Blocks(blocks)) = message.content else {
        return None;
    };
    let text = blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text),
            ContentBlock::Other => None,
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

/// The envelope of one line of pi's RPC output.
#[derive(Deserialize)]
struct RpcEnvelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    success: bool,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    data: T,
}

#[derive(Deserialize)]
struct AvailableModels {
    models: Vec<ListedModel>,
}

/// A model pi cannot describe is skipped rather than failing discovery.
#[derive(Deserialize)]
#[serde(untagged)]
enum ListedModel {
    Known(RpcModel),
    Unknown(IgnoredAny),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcModel {
    id: String,
    provider: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    reasoning: bool,
    /// A level mapped to `null` is unsupported by the model.
    #[serde(default)]
    thinking_level_map: BTreeMap<String, Option<IgnoredAny>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionState {
    #[serde(default)]
    model: Option<ModelRef>,
    #[serde(default)]
    thinking_level: Option<String>,
}

#[derive(Deserialize)]
struct ModelRef {
    provider: String,
    id: String,
}

/// Parse the output of [`WORKSPACE_MODEL_DISCOVERY_SCRIPT`].
///
/// Model ids are `provider/id`, as pi's `--model` takes them. Thinking
/// levels follow pi's rules: a level mapped to `null` is unsupported, `xhigh`
/// and `max` exist only where a model maps them, and a model without
/// reasoning has none to choose. pi's default model comes first.
pub fn parse_model_capabilities_from_rpc_output(output: &str) -> Vec<AiCliModelCapability> {
    let mut models = None;
    let mut state = None;
    for line in output.lines() {
        let Ok(envelope) = serde_json::from_str::<RpcEnvelope>(line) else {
            continue;
        };
        if envelope.kind != "response" || !envelope.success {
            continue;
        }
        match envelope.id.as_deref() {
            Some(MODELS_RESPONSE_ID) if models.is_none() => {
                models = serde_json::from_str::<RpcResponse<AvailableModels>>(line)
                    .ok()
                    .map(|response| response.data.models);
            }
            Some(STATE_RESPONSE_ID) if state.is_none() => {
                state = serde_json::from_str::<RpcResponse<SessionState>>(line)
                    .ok()
                    .map(|response| response.data);
            }
            _ => {}
        }
    }
    let Some(models) = models else {
        return Vec::new();
    };
    let default_model = state
        .as_ref()
        .and_then(|state| state.model.as_ref())
        .and_then(|model| model_id(&model.provider, &model.id));
    let default_level = state
        .as_ref()
        .and_then(|state| state.thinking_level.as_deref());

    let mut capabilities = Vec::new();
    for model in models.iter().take(MAX_DISCOVERED_MODELS) {
        let ListedModel::Known(model) = model else {
            continue;
        };
        let Some(id) = model_id(&model.provider, &model.id) else {
            continue;
        };
        if capabilities
            .iter()
            .any(|existing: &AiCliModelCapability| existing.id == id)
        {
            continue;
        }
        let is_default = default_model.as_deref() == Some(id.as_str());
        let levels = supported_thinking_levels(model);
        let default_reasoning_option = clamp_thinking_level(
            &levels,
            default_level
                .filter(|_| is_default)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
        )
        .map(str::to_string);
        let name = model
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
            .map(|name| name.chars().take(MAX_MODEL_NAME_CHARS).collect::<String>())
            .unwrap_or_else(|| id.clone());
        let capability = AiCliModelCapability {
            id,
            name,
            reasoning_options: levels.iter().map(|level| (*level).to_string()).collect(),
            default_reasoning_option: (!levels.is_empty())
                .then_some(default_reasoning_option)
                .flatten(),
        };
        if is_default {
            capabilities.insert(0, capability);
        } else {
            capabilities.push(capability);
        }
    }
    capabilities
}

/// `provider/id`, when both parts are present and safe to pass to `--model`.
fn model_id(provider: &str, id: &str) -> Option<String> {
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with('-')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@+".contains(&byte))
    };
    let full = format!("{provider}/{id}");
    (valid(provider) && valid(id) && full.len() <= MAX_MODEL_ID_CHARS).then_some(full)
}

fn supported_thinking_levels(model: &RpcModel) -> Vec<&'static str> {
    if !model.reasoning {
        return Vec::new();
    }
    THINKING_LEVELS
        .into_iter()
        .filter(|level| match model.thinking_level_map.get(*level) {
            Some(None) => false,
            Some(Some(_)) => true,
            None => !matches!(*level, "xhigh" | "max"),
        })
        .collect()
}

/// pi's own clamping: the requested level, else the next higher supported
/// one, else the highest supported one.
fn clamp_thinking_level<'a>(levels: &[&'a str], requested: &str) -> Option<&'a str> {
    if let Some(level) = levels.iter().find(|level| **level == requested) {
        return Some(level);
    }
    let start = THINKING_LEVELS
        .iter()
        .position(|level| *level == requested)?;
    THINKING_LEVELS[start..]
        .iter()
        .find_map(|candidate| levels.iter().find(|level| *level == candidate).copied())
        .or_else(|| levels.last().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn discovery_output(models: serde_json::Value, state: serde_json::Value) -> String {
        [
            json!({"type": "extension_ui_request", "method": "notify"}),
            json!({"id": "temps-models", "type": "response", "command": "get_available_models",
                "success": true, "data": {"models": models}}),
            json!({"id": "temps-state", "type": "response", "command": "get_state",
                "success": true, "data": state}),
        ]
        .iter()
        .map(serde_json::Value::to_string)
        .chain(["not json".to_string()])
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn discovery_lists_provider_models_with_pi_thinking_levels() {
        let output = discovery_output(
            json!([
                {"id": "claude-sonnet-4-5", "name": "Claude Sonnet 4.5", "provider": "anthropic",
                    "reasoning": true, "thinkingLevelMap": {}},
                {"id": "claude-opus-4-8", "name": "Claude Opus 4.8", "provider": "anthropic",
                    "reasoning": true, "thinkingLevelMap": {"off": null, "xhigh": "xhigh", "max": "max"}},
                {"id": "claude-haiku-4-5", "name": "", "provider": "anthropic", "reasoning": false},
            ]),
            json!({"model": {"provider": "anthropic", "id": "claude-opus-4-8"}, "thinkingLevel": "high"}),
        );
        let models = parse_model_capabilities_from_rpc_output(&output);
        assert_eq!(
            models,
            [
                AiCliModelCapability {
                    id: "anthropic/claude-opus-4-8".into(),
                    name: "Claude Opus 4.8".into(),
                    reasoning_options: ["minimal", "low", "medium", "high", "xhigh", "max"]
                        .map(String::from)
                        .to_vec(),
                    default_reasoning_option: Some("high".into()),
                },
                AiCliModelCapability {
                    id: "anthropic/claude-sonnet-4-5".into(),
                    name: "Claude Sonnet 4.5".into(),
                    reasoning_options: ["off", "minimal", "low", "medium", "high"]
                        .map(String::from)
                        .to_vec(),
                    default_reasoning_option: Some("medium".into()),
                },
                AiCliModelCapability {
                    id: "anthropic/claude-haiku-4-5".into(),
                    name: "anthropic/claude-haiku-4-5".into(),
                    reasoning_options: Vec::new(),
                    default_reasoning_option: None,
                },
            ]
        );
    }

    #[test]
    fn discovery_skips_ids_pi_could_misread_and_duplicates() {
        let output = discovery_output(
            json!([
                {"id": "--model", "provider": "anthropic"},
                {"id": "ok", "provider": "anthropic"},
                {"id": "ok", "provider": "anthropic"},
                {"id": "has space", "provider": "anthropic"},
                {"id": "x", "provider": ""},
                {"id": "y"},
            ]),
            json!({}),
        );
        let ids = parse_model_capabilities_from_rpc_output(&output)
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["anthropic/ok"]);
    }

    #[test]
    fn a_missing_or_failed_model_response_discovers_nothing() {
        assert!(parse_model_capabilities_from_rpc_output("").is_empty());
        let failed =
            json!({"id": "temps-models", "type": "response", "command": "get_available_models",
            "success": false, "error": "boom"})
            .to_string();
        assert!(parse_model_capabilities_from_rpc_output(&failed).is_empty());
    }

    #[test]
    fn clamping_matches_pi() {
        let levels = ["minimal", "low", "high"];
        assert_eq!(clamp_thinking_level(&levels, "low"), Some("low"));
        assert_eq!(clamp_thinking_level(&levels, "medium"), Some("high"));
        assert_eq!(clamp_thinking_level(&levels, "max"), Some("high"));
        assert_eq!(clamp_thinking_level(&levels, "unknown"), None);
        assert_eq!(clamp_thinking_level(&[], "medium"), None);
    }

    #[tokio::test]
    async fn host_execution_fails_with_an_explicit_error() {
        let config = AiRunConfig {
            work_dir: std::env::temp_dir(),
            prompt: "hello".into(),
            api_key: String::new(),
            max_turns: 1,
            timeout: Duration::from_secs(1),
            model: None,
            thinking_level: None,
            permission_mode: None,
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };
        let Err(error) = PiCliProvider.run(config).await else {
            panic!("pi must not run on the host");
        };
        assert!(matches!(
            &error,
            AgentError::AiCliWorkspaceChatOnly { provider, .. } if provider == "pi"
        ));
        assert!(error.to_string().contains("host execution"), "{error}");
    }

    #[test]
    fn assistant_text_comes_only_from_completed_assistant_messages() {
        let message = |role: &str, stop: &str, content: serde_json::Value| {
            json!({"type": "message_end", "message": {"role": role, "stopReason": stop, "content": content}})
                .to_string()
        };
        let text = json!([{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": "OK"}]);
        assert_eq!(
            extract_assistant_text(&message("assistant", "stop", text.clone())).as_deref(),
            Some("OK")
        );
        assert_eq!(
            extract_assistant_text(&message("user", "stop", text.clone())),
            None
        );
        assert_eq!(
            extract_assistant_text(&message("assistant", "error", text)),
            None
        );
        assert_eq!(
            extract_assistant_text(&message("assistant", "stop", json!([]))),
            None
        );
        assert_eq!(
            extract_assistant_text(&json!({"type": "message_update"}).to_string()),
            None
        );
        assert_eq!(extract_assistant_text("not json"), None);
    }

    #[test]
    fn agent_files_route_models_to_the_relay_and_keep_the_mcp_token_in_env() {
        fn encoded(config: impl Serialize) -> serde_json::Value {
            serde_json::to_value(config).expect("pi agent files serialize")
        }
        assert_eq!(
            encoded(models_config("anthropic", "http://relay.test/r1")),
            json!({"providers": {"anthropic": {
                "baseUrl": "http://relay.test/r1",
                "apiKey": "$TEMPS_PI_MODEL_RELAY_TOKEN",
                "headers": {"Authorization": "Bearer ${TEMPS_PI_MODEL_RELAY_TOKEN}"},
            }}})
        );
        assert_eq!(
            encoded(models_config("openai", "http://relay.test/r2")),
            json!({"providers": {"openai": {
                "baseUrl": "http://relay.test/r2",
                "apiKey": "$TEMPS_PI_MODEL_RELAY_TOKEN",
            }}})
        );
        assert_eq!(encoded(mcp_config(None)), json!({"mcpServers": {}}));
        assert_eq!(
            encoded(mcp_config(Some("http://mcp.test/turn"))),
            json!({"mcpServers": {"temps-chat": {
                "url": "http://mcp.test/turn",
                "headers": {"Authorization": "${TEMPS_CHAT_MCP_AUTHORIZATION}"},
                "exposure": "direct",
            }}})
        );
        assert_eq!(workspace_agent_dir(), "/home/temps/.temps-pi/agent");
    }

    #[test]
    fn verification_disables_tools_and_ambient_configuration() {
        let command = verification_command("anthropic/claude-haiku-4-5");
        for flag in [
            "--print",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--no-approve",
        ] {
            assert!(command.iter().any(|arg| arg == flag), "missing {flag}");
        }
        assert_eq!(command.last().map(String::as_str), Some("Reply OK."));
        assert_eq!(command[command.len() - 2], "--");
    }

    #[test]
    fn the_discovery_script_asks_for_models_and_state_without_a_session() {
        assert!(WORKSPACE_MODEL_DISCOVERY_SCRIPT.contains(MODELS_RESPONSE_ID));
        assert!(WORKSPACE_MODEL_DISCOVERY_SCRIPT.contains(STATE_RESPONSE_ID));
        assert!(WORKSPACE_MODEL_DISCOVERY_SCRIPT.contains("exec pi --mode rpc --no-session"));
        assert!(WORKSPACE_MODEL_DISCOVERY_SCRIPT.contains("--no-extensions"));
    }
}
