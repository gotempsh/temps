// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::{
    copy_environment_variable, sanitize_command_environment, AiCliProvider, AiCliStatus,
    AiRunConfig, AiRunResult,
};
use crate::error::AgentError;

const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MODEL_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct OpenCodeCliProvider;

fn cancellation_safe_command() -> Command {
    let mut command = Command::new("opencode");
    sanitize_command_environment(&mut command);
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
        "DEEPSEEK_API_KEY",
    ] {
        copy_environment_variable(&mut command, name);
    }
    command.kill_on_drop(true);
    command
}

fn apply_chat_mcp(cmd: &mut Command, config: &AiRunConfig) {
    let mut runtime_config = serde_json::json!({
        // Tool-less completions and chat runs must not inherit the selected
        // host agent's bash/filesystem/web permissions. A prompt can contain
        // untrusted product data even when the server authored its template.
        "permission": { "*": "deny" }
    });
    if let Some(server) = &config.mcp_server {
        runtime_config["mcp"] = serde_json::json!({
            "temps-chat": {
                    "type": "remote",
                    "url": server.url,
                    "headers": { "Authorization": format!("Bearer {}", server.authorization_token) }
            }
        });
        runtime_config["permission"]["temps_chat_*"] = serde_json::json!("allow");
    }
    cmd.env("OPENCODE_CONFIG_CONTENT", runtime_config.to_string());
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeModelInfo {
    pub id: String,
    pub name: String,
    pub variants: Vec<String>,
}

fn parse_model_listing(output: &str) -> Vec<OpenCodeModelInfo> {
    let mut models = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = output[cursor..].find('{') {
        let json_start = cursor + relative_start;
        let header = output[cursor..json_start].trim();
        let Some(id) = header
            .lines()
            .last()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            break;
        };
        let mut values = serde_json::Deserializer::from_str(&output[json_start..])
            .into_iter::<serde_json::Value>();
        let Some(Ok(value)) = values.next() else {
            break;
        };
        let consumed = values.byte_offset();
        if consumed == 0 {
            break;
        }
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(id)
            .to_string();
        let variants = value
            .get("variants")
            .and_then(serde_json::Value::as_object)
            .map(|variants| variants.keys().cloned().collect())
            .unwrap_or_default();
        models.push(OpenCodeModelInfo {
            id: id.to_string(),
            name,
            variants,
        });
        cursor = json_start + consumed;
    }
    models
}

/// Ask OpenCode for models available to its configured providers. `--verbose`
/// supplies display names and the exact per-model variant set accepted by
/// `opencode run --variant`.
pub async fn discover_models() -> Vec<OpenCodeModelInfo> {
    let mut command = cancellation_safe_command();
    command
        .args(["models", "--verbose"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(MODEL_DISCOVERY_TIMEOUT, command.output())
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|output| output.status.success())
        .map(|output| parse_model_listing(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

#[async_trait]
impl AiCliProvider for OpenCodeCliProvider {
    fn name(&self) -> &str {
        "opencode"
    }

    async fn check_installed(&self) -> bool {
        tokio::time::timeout(
            STATUS_TIMEOUT,
            cancellation_safe_command()
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(|status| status.success())
        .unwrap_or(false)
    }

    async fn get_status(&self) -> AiCliStatus {
        let version_output = tokio::time::timeout(
            STATUS_TIMEOUT,
            cancellation_safe_command()
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        let (installed, version) = match version_output {
            Ok(Ok(output)) if output.status.success() => {
                let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (true, if ver.is_empty() { None } else { Some(ver) })
            }
            _ => {
                return AiCliStatus {
                    provider: "opencode".into(),
                    installed: false,
                    version: None,
                    authenticated: false,
                    auth_method: None,
                    email: None,
                    subscription_type: None,
                    setup_hint: Some(
                        "Install OpenCode: curl -fsSL https://opencode.ai/install | bash".into(),
                    ),
                };
            }
        };

        // Ask OpenCode to inspect its own auth store rather than reading
        // auth.json. This respects the runtime user's HOME/XDG paths and keeps
        // credential contents outside Temps. Current OpenCode exposes `auth
        // list`; older code used the nonexistent `auth status` subcommand and
        // therefore reported every host login as unavailable.
        let auth_output = tokio::time::timeout(
            STATUS_TIMEOUT,
            cancellation_safe_command()
                .args(["auth", "list"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        let (authenticated, setup_hint) = match auth_output {
            Ok(Ok(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !opencode_has_credentials(&stdout) {
                    (
                        false,
                        Some(
                            "Run 'opencode auth login' as the user running Temps to configure a provider."
                                .into(),
                        ),
                    )
                } else {
                    (true, None)
                }
            }
            _ => (
                false,
                Some(
                    "Run 'opencode auth login' as the user running Temps to configure a provider."
                        .into(),
                ),
            ),
        };

        AiCliStatus {
            provider: "opencode".into(),
            installed,
            version,
            authenticated,
            auth_method: authenticated.then(|| "host_auth_store".to_string()),
            email: None,
            subscription_type: None,
            setup_hint,
        }
    }

    async fn discover_model_capabilities(&self) -> Vec<super::AiCliModelCapability> {
        discover_models()
            .await
            .into_iter()
            .map(|model| {
                let default_reasoning_option = model
                    .variants
                    .iter()
                    .find(|variant| variant.as_str() == "medium")
                    .or_else(|| model.variants.first())
                    .cloned();
                super::AiCliModelCapability {
                    id: model.id,
                    name: model.name,
                    reasoning_options: model.variants,
                    default_reasoning_option,
                }
            })
            .collect()
    }

    fn extract_assistant_text(&self, line: &str) -> Option<String> {
        extract_assistant_text(line)
    }

    fn dropped_tool_use_name(&self, line: &str) -> Option<String> {
        dropped_tool_use_name(line)
    }

    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = cancellation_safe_command();
        cmd.arg("run");
        if let Some(agent) = config.permission_mode.as_deref() {
            cmd.arg("--agent").arg(agent);
        }
        if let Some(variant) = config.thinking_level.as_deref() {
            if variant != "default" {
                cmd.arg("--variant").arg(variant);
            }
        }
        // Flags must come *before* the positional message, since everything
        // after the first positional is treated as part of the prompt.
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        apply_chat_mcp(&mut cmd, &config);
        cmd.arg("--format")
            .arg("json")
            .arg(&config.prompt)
            .current_dir(&config.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !config.api_key.is_empty() {
            cmd.env(env_var_for_model(config.model.as_deref()), &config.api_key);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::AiCliNotInstalled {
                    provider: self.name().to_string(),
                }
            } else {
                AgentError::Io(e)
            }
        })?;

        let stdout_handle = child.stdout.take().expect("stdout was piped");
        let stderr_handle = child.stderr.take().expect("stderr was piped");
        let on_event = config.on_event.clone();

        let stream_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout_handle);
            let mut lines = reader.lines();
            let mut all_output = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                all_output.push_str(&line);
                all_output.push('\n');

                if let Some(ref cb) = on_event {
                    cb(line).await;
                }
            }

            all_output
        });

        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr_handle);
            let mut lines = reader.lines();
            let mut all_stderr = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                all_stderr.push_str(&line);
                all_stderr.push('\n');
            }

            all_stderr
        });

        let wait_result = tokio::time::timeout(config.timeout, child.wait()).await;

        let status = match wait_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(AgentError::Io(e)),
            Err(_) => {
                let _ = child.kill().await;
                return Err(AgentError::AiCliTimeout {
                    provider: self.name().to_string(),
                    timeout_secs: config.timeout.as_secs(),
                });
            }
        };

        let stdout = stream_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let exit_code = status.code().unwrap_or(-1);

        if !status.success() {
            let error_output = if stderr.trim().is_empty() {
                stdout.clone()
            } else {
                stderr
            };
            return Err(AgentError::AiCliFailed {
                provider: self.name().to_string(),
                exit_code,
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &error_output),
            });
        }

        if output_reports_error(&stdout) {
            return Err(AgentError::AiCliReportedError {
                provider: self.name().to_string(),
                message: crate::ai_cli::summarize_cli_failure(self.name(), &stdout),
            });
        }

        // Parse OpenCode JSON output for token usage
        let (tokens_input, tokens_output, model) = parse_opencode_output(&stdout);

        Ok(AiRunResult {
            output: stdout,
            exit_code,
            tokens_input,
            tokens_output,
            model,
            changed_files: None,
            session_id: None,
            is_max_turns_error: false,
        })
    }

    async fn continue_conversation(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = cancellation_safe_command();
        cmd.arg("run").arg("--continue");
        if let Some(agent) = config.permission_mode.as_deref() {
            cmd.arg("--agent").arg(agent);
        }
        if let Some(variant) = config.thinking_level.as_deref() {
            if variant != "default" {
                cmd.arg("--variant").arg(variant);
            }
        }
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        apply_chat_mcp(&mut cmd, &config);
        cmd.arg("--format")
            .arg("json")
            .arg(&config.prompt)
            .current_dir(&config.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !config.api_key.is_empty() {
            cmd.env(env_var_for_model(config.model.as_deref()), &config.api_key);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::AiCliNotInstalled {
                    provider: self.name().to_string(),
                }
            } else {
                AgentError::Io(e)
            }
        })?;

        let stdout_handle = child.stdout.take().expect("stdout was piped");
        let stderr_handle = child.stderr.take().expect("stderr was piped");
        let on_event = config.on_event.clone();

        let stream_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout_handle);
            let mut lines = reader.lines();
            let mut all_output = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                all_output.push_str(&line);
                all_output.push('\n');

                if let Some(ref cb) = on_event {
                    cb(line).await;
                }
            }

            all_output
        });

        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr_handle);
            let mut lines = reader.lines();
            let mut all_stderr = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                all_stderr.push_str(&line);
                all_stderr.push('\n');
            }

            all_stderr
        });

        let wait_result = tokio::time::timeout(config.timeout, child.wait()).await;

        let status = match wait_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(AgentError::Io(e)),
            Err(_) => {
                let _ = child.kill().await;
                return Err(AgentError::AiCliTimeout {
                    provider: self.name().to_string(),
                    timeout_secs: config.timeout.as_secs(),
                });
            }
        };

        let stdout = stream_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let exit_code = status.code().unwrap_or(-1);

        if !status.success() {
            let error_output = if stderr.trim().is_empty() {
                stdout.clone()
            } else {
                stderr
            };
            return Err(AgentError::AiCliFailed {
                provider: self.name().to_string(),
                exit_code,
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &error_output),
            });
        }

        if output_reports_error(&stdout) {
            return Err(AgentError::AiCliReportedError {
                provider: self.name().to_string(),
                message: crate::ai_cli::summarize_cli_failure(self.name(), &stdout),
            });
        }

        let (tokens_input, tokens_output, model) = parse_opencode_output(&stdout);

        Ok(AiRunResult {
            output: stdout,
            exit_code,
            tokens_input,
            tokens_output,
            model,
            changed_files: None,
            session_id: None,
            is_max_turns_error: false,
        })
    }
}

fn opencode_has_credentials(status_output: &str) -> bool {
    let normalized = status_output.trim().to_ascii_lowercase();
    !normalized.is_empty()
        && !normalized.contains("no credentials")
        && !normalized.contains("0 credentials")
}

fn output_reports_error(output: &str) -> bool {
    output.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
            .as_deref()
            == Some("error")
    })
}

/// Parse OpenCode `--format json` output for accumulated token usage and
/// model info.
///
/// OpenCode emits one JSON object per line. Token counts live on
/// `step_finish` events at `part.tokens.{input,output}`, and the model
/// identifier appears on `step_start` events at `part.providerID` +
/// `part.modelID` (combined as `provider/model`). There may be multiple
/// `step_finish` events per run (one per reasoning/tool step), so we sum
/// the counts.
pub fn parse_opencode_output(output: &str) -> (Option<i32>, Option<i32>, Option<String>) {
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut saw_tokens = false;
    let mut model: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if event_type == "step_finish" {
            if let Some(tokens) = v.get("part").and_then(|p| p.get("tokens")) {
                if let Some(input) = tokens.get("input").and_then(|n| n.as_i64()) {
                    total_input += input;
                    saw_tokens = true;
                }
                if let Some(output) = tokens.get("output").and_then(|n| n.as_i64()) {
                    total_output += output;
                    saw_tokens = true;
                }
            }
        }

        // Capture the model from the first event that mentions it.
        if model.is_none() {
            if let Some(part) = v.get("part") {
                let provider = part.get("providerID").and_then(|v| v.as_str());
                let model_id = part.get("modelID").and_then(|v| v.as_str());
                match (provider, model_id) {
                    (Some(p), Some(m)) => model = Some(format!("{}/{}", p, m)),
                    (_, Some(m)) => model = Some(m.to_string()),
                    _ => {}
                }
            }
            if model.is_none() {
                model = v.get("model").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }

    let tokens_input = if saw_tokens {
        Some(total_input as i32)
    } else {
        None
    };
    let tokens_output = if saw_tokens {
        Some(total_output as i32)
    } else {
        None
    };

    (tokens_input, tokens_output, model)
}

/// Map an OpenCode `provider/model` identifier to the env var OpenCode
/// expects for that provider's credential. Falls back to
/// `ANTHROPIC_API_KEY` when the provider prefix is missing or unknown —
/// matches historical behavior.
fn env_var_for_model(model: Option<&str>) -> &'static str {
    let provider = model
        .and_then(|m| m.split_once('/').map(|(p, _)| p))
        .unwrap_or("");
    match provider {
        "openai" | "openrouter" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "google" | "gemini" => "GOOGLE_API_KEY",
        "groq" => "GROQ_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "xai" => "XAI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        _ => "ANTHROPIC_API_KEY",
    }
}

/// Extract assistant text from one line of OpenCode's `--format json`
/// output. Only `type":"text"` events (with `part.type":"text"`) carry
/// user-facing text — `step_start`/`step_finish`/tool events return `None`.
pub fn extract_assistant_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("text") {
        return None;
    }
    let text = value.get("part")?.get("text").and_then(|v| v.as_str())?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Name the tool of a `type":"tool"` event, when `line` reports a tool
/// invocation OpenCode's CLI cannot bridge back to the user in CLI-chat
/// mode. Returns `None` for text/step events and any other event shape (see
/// [`extract_assistant_text`]).
///
/// Used only for diagnostic logging (ADR-038) — never logs tool arguments or
/// output, which may carry user data.
pub fn dropped_tool_use_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("tool") {
        return None;
    }
    value
        .get("part")
        .and_then(|p| p.get("tool"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some("tool".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_models_and_exact_variants_from_verbose_listing() {
        let output = r#"openai/gpt-5.4
{
  "name": "GPT-5.4",
  "variants": {
    "none": { "reasoningEffort": "none" },
    "low": { "reasoningEffort": "low" },
    "medium": { "reasoningEffort": "medium" },
    "high": { "reasoningEffort": "high" }
  }
}
ollama/qwen3.5:9b
{
  "name": "qwen3.5:9b",
  "variants": {}
}"#;

        let models = parse_model_listing(output);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "openai/gpt-5.4");
        assert_eq!(models[0].name, "GPT-5.4");
        assert_eq!(models[0].variants, ["none", "low", "medium", "high"]);
        assert_eq!(models[1].id, "ollama/qwen3.5:9b");
        assert!(models[1].variants.is_empty());
    }

    #[test]
    fn test_opencode_provider_name() {
        let provider = OpenCodeCliProvider;
        assert_eq!(provider.name(), "opencode");
    }

    #[test]
    fn opencode_registers_v2_remote_chat_mcp_config() {
        let config = AiRunConfig {
            work_dir: std::env::temp_dir(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: Some("build".to_string()),
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: Some(crate::ai_cli::McpServerConfig {
                url: "http://127.0.0.1:32123/mcp/turn".to_string(),
                authorization_token: "ephemeral-test-token".to_string(),
            }),
        };
        let mut command = cancellation_safe_command();
        apply_chat_mcp(&mut command, &config);
        let value = command
            .as_std()
            .get_envs()
            .find_map(|(name, value)| {
                (name == "OPENCODE_CONFIG_CONTENT")
                    .then(|| value.map(|value| value.to_string_lossy().into_owned()))
                    .flatten()
            })
            .expect("OpenCode config env");
        let json: serde_json::Value = serde_json::from_str(&value).expect("parse config JSON");
        assert_eq!(json["mcp"]["temps-chat"]["type"], "remote");
        assert!(json["mcp"]["temps-chat"].get("codemode").is_none());
        assert!(json["mcp"]["temps-chat"].get("enabled").is_none());
        assert_eq!(json["permission"]["*"], "deny");
        assert_eq!(json["permission"]["temps_chat_*"], "allow");
    }

    #[test]
    fn opencode_toolless_completion_denies_native_host_tools() {
        let config = AiRunConfig {
            work_dir: std::env::temp_dir(),
            prompt: "summarize numbers".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: None,
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };
        let mut command = cancellation_safe_command();
        apply_chat_mcp(&mut command, &config);
        let value = command
            .as_std()
            .get_envs()
            .find_map(|(name, value)| {
                (name == "OPENCODE_CONFIG_CONTENT")
                    .then(|| value.map(|value| value.to_string_lossy().into_owned()))
                    .flatten()
            })
            .expect("OpenCode config env");
        let json: serde_json::Value = serde_json::from_str(&value).expect("parse config JSON");
        assert_eq!(json["permission"]["*"], "deny");
        assert!(json.get("mcp").is_none());
    }

    #[test]
    fn test_opencode_auth_list_detects_credentials_without_reading_secrets() {
        let output = "Credentials ~/.local/share/opencode/auth.json\nOpenAI oauth\n2 credentials";
        assert!(opencode_has_credentials(output));
        assert!(!opencode_has_credentials("No credentials"));
        assert!(!opencode_has_credentials("0 credentials"));
        assert!(!opencode_has_credentials("  "));
    }

    #[test]
    fn test_parse_opencode_output_empty() {
        let (input, output, model) = parse_opencode_output("");
        assert!(input.is_none());
        assert!(output.is_none());
        assert!(model.is_none());
    }

    #[test]
    fn detects_error_event_even_when_opencode_exits_zero() {
        let output = r#"{"type":"error","timestamp":1,"error":{"data":{"message":"Token refresh failed: 401"}}}"#;
        assert!(output_reports_error(output));
        assert!(!output_reports_error(
            r#"{"type":"text","part":{"type":"text","text":"4"}}"#
        ));
    }

    #[test]
    fn test_parse_opencode_output_with_usage() {
        // Real OpenCode `--format json` shape: tokens on step_finish.part.tokens,
        // model assembled from providerID/modelID on step_start.part.
        let output = r#"{"type":"step_start","part":{"type":"step-start","providerID":"anthropic","modelID":"claude-sonnet-4-20250514"}}
{"type":"text","part":{"type":"text","text":"hello"}}
{"type":"step_finish","part":{"type":"step-finish","reason":"stop","tokens":{"input":1000,"output":500},"cost":0.01}}"#;
        let (input, output_tokens, model) = parse_opencode_output(output);
        assert_eq!(input, Some(1000));
        assert_eq!(output_tokens, Some(500));
        assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4-20250514"));
    }

    #[test]
    fn test_parse_opencode_output_accumulates_multiple_steps() {
        let output = r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":100,"output":50}}}
{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":200,"output":75}}}"#;
        let (input, output_tokens, _model) = parse_opencode_output(output);
        assert_eq!(input, Some(300));
        assert_eq!(output_tokens, Some(125));
    }

    #[test]
    fn test_env_var_for_model() {
        assert_eq!(env_var_for_model(Some("openai/gpt-5")), "OPENAI_API_KEY");
        assert_eq!(
            env_var_for_model(Some("anthropic/claude-sonnet-4")),
            "ANTHROPIC_API_KEY"
        );
        assert_eq!(
            env_var_for_model(Some("google/gemini-2.0")),
            "GOOGLE_API_KEY"
        );
        // Unknown provider → Anthropic fallback (matches pre-fix default).
        assert_eq!(env_var_for_model(Some("unknown/x")), "ANTHROPIC_API_KEY");
        assert_eq!(env_var_for_model(None), "ANTHROPIC_API_KEY");
        // No slash → also falls back.
        assert_eq!(env_var_for_model(Some("bare-model")), "ANTHROPIC_API_KEY");
    }

    #[test]
    fn test_extract_assistant_text_from_text_event() {
        let line = r#"{"type":"text","part":{"type":"text","text":"hello"}}"#;
        assert_eq!(extract_assistant_text(line).as_deref(), Some("hello"));
    }

    #[test]
    fn test_extract_assistant_text_ignores_step_events() {
        let step_start =
            r#"{"type":"step_start","part":{"type":"step-start","providerID":"anthropic"}}"#;
        let step_finish =
            r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":1}}}"#;
        assert_eq!(extract_assistant_text(step_start), None);
        assert_eq!(extract_assistant_text(step_finish), None);
    }

    #[test]
    fn test_dropped_tool_use_name_from_tool_event() {
        let line = r#"{"type":"tool","part":{"tool":"bash","input":{"command":"rm -rf /secret"}}}"#;
        let name = dropped_tool_use_name(line);
        assert_eq!(name.as_deref(), Some("bash"));
        assert!(!name.unwrap().contains("rm -rf"));
    }

    #[test]
    fn test_dropped_tool_use_name_falls_back_when_tool_name_missing() {
        let line = r#"{"type":"tool","part":{}}"#;
        assert_eq!(dropped_tool_use_name(line).as_deref(), Some("tool"));
    }

    #[test]
    fn test_dropped_tool_use_name_none_for_text_and_step_events() {
        let text = r#"{"type":"text","part":{"type":"text","text":"hi"}}"#;
        let step_start = r#"{"type":"step_start","part":{"type":"step-start"}}"#;
        assert_eq!(dropped_tool_use_name(text), None);
        assert_eq!(dropped_tool_use_name(step_start), None);
    }
}
