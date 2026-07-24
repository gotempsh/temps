use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::{AiCliProvider, AiCliStatus, AiRunConfig, AiRunResult};
use crate::error::AgentError;

pub struct OpenCodeCliProvider;

#[async_trait]
impl AiCliProvider for OpenCodeCliProvider {
    fn name(&self) -> &str {
        "opencode"
    }

    async fn check_installed(&self) -> bool {
        Command::new("opencode")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn get_status(&self) -> AiCliStatus {
        let version_output = Command::new("opencode")
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let (installed, version) = match version_output {
            Ok(output) if output.status.success() => {
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

        // OpenCode uses API keys configured via `opencode auth`
        // Check if any provider is configured by running `opencode models`
        let auth_output = Command::new("opencode")
            .args(["auth", "status"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let (authenticated, setup_hint) = match auth_output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.contains("No credentials") || stdout.trim().is_empty() {
                    (
                        false,
                        Some("Run 'opencode auth add' to configure an AI provider API key.".into()),
                    )
                } else {
                    (true, None)
                }
            }
            _ => (
                false,
                Some("Run 'opencode auth add' to configure an AI provider API key.".into()),
            ),
        };

        AiCliStatus {
            provider: "opencode".into(),
            installed,
            version,
            authenticated,
            auth_method: None,
            email: None,
            subscription_type: None,
            setup_hint,
        }
    }

    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = Command::new("opencode");
        cmd.arg("run");
        // Flags must come *before* the positional message, since everything
        // after the first positional is treated as part of the prompt.
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
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

        let mut cmd = Command::new("opencode");
        cmd.arg("run").arg("--continue");
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opencode_provider_name() {
        let provider = OpenCodeCliProvider;
        assert_eq!(provider.name(), "opencode");
    }

    #[test]
    fn test_parse_opencode_output_empty() {
        let (input, output, model) = parse_opencode_output("");
        assert!(input.is_none());
        assert!(output.is_none());
        assert!(model.is_none());
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
}
