use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::{AiCliProvider, AiCliStatus, AiRunConfig, AiRunResult};
use crate::error::AgentError;
use crate::sandbox::user::SANDBOX_USER;

/// Check if a Unix user exists by name.
fn user_exists(name: &str) -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub struct ClaudeCliProvider;

#[async_trait]
impl AiCliProvider for ClaudeCliProvider {
    fn name(&self) -> &str {
        "claude_cli"
    }

    async fn check_installed(&self) -> bool {
        Command::new("claude")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn get_status(&self) -> AiCliStatus {
        // Check if installed
        let version_output = Command::new("claude")
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
                    provider: "claude_cli".into(),
                    installed: false,
                    version: None,
                    authenticated: false,
                    auth_method: None,
                    email: None,
                    subscription_type: None,
                    setup_hint: Some(
                        "Install Claude CLI: npm install -g @anthropic-ai/claude-code".into(),
                    ),
                };
            }
        };

        // Check auth status
        let auth_output = Command::new("claude")
            .args(["auth", "status"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let (authenticated, auth_method, email, subscription_type, setup_hint) = match auth_output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let logged_in = json.get("loggedIn").and_then(|v| v.as_bool()).unwrap_or(false);
                    if logged_in {
                        (
                            true,
                            json.get("authMethod").and_then(|v| v.as_str()).map(String::from),
                            json.get("email").and_then(|v| v.as_str()).map(String::from),
                            json.get("subscriptionType").and_then(|v| v.as_str()).map(String::from),
                            None,
                        )
                    } else {
                        (false, None, None, None, Some(
                            "Run 'claude setup-token' on the server to authenticate, or set ANTHROPIC_API_KEY. Configure in Settings > AI Agents.".into(),
                        ))
                    }
                } else {
                    (false, None, None, None, Some(
                        "Run 'claude setup-token' on the server to authenticate.".into(),
                    ))
                }
            }
            _ => (false, None, None, None, Some(
                "Run 'claude setup-token' on the server to authenticate, or set ANTHROPIC_API_KEY. Configure in Settings > AI Agents.".into(),
            )),
        };

        AiCliStatus {
            provider: "claude_cli".into(),
            installed,
            version,
            authenticated,
            auth_method,
            email,
            subscription_type,
            setup_hint,
        }
    }

    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let is_root = unsafe { libc::geteuid() } == 0;

        // Build the claude argv exactly the same in both branches. Each
        // arg goes through Command::arg, so no shell ever parses
        // user-controlled prompts/model/api_key data — values stay in
        // their own argv slot and are passed straight to execve().
        //
        // When running as root, Claude CLI refuses
        // --dangerously-skip-permissions. We drop privileges via
        // `runuser -u <user> -- claude <args...>`. `runuser` is the
        // recommended replacement for `su -c`: it preserves separate
        // argv slots, has no shell-string parameter, and is available
        // on every Linux distro temps targets (util-linux).
        let mut cmd = if is_root {
            let run_user = if user_exists(SANDBOX_USER) {
                SANDBOX_USER
            } else {
                "nobody"
            };
            let mut c = Command::new("runuser");
            c.arg("-u").arg(run_user).arg("--").arg("claude");
            c
        } else {
            Command::new("claude")
        };
        cmd.arg("--print")
            .arg(&config.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--max-turns")
            .arg(config.max_turns.to_string())
            .arg("--dangerously-skip-permissions")
            .arg("--verbose");
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        cmd.current_dir(&config.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !config.api_key.is_empty() {
            cmd.env("ANTHROPIC_API_KEY", &config.api_key);
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

        // Stream stdout line by line, calling on_event for each JSON line
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

                // Call the real-time callback if provided
                if let Some(ref cb) = on_event {
                    cb(line).await;
                }
            }

            all_output
        });

        // Capture stderr in parallel
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

        // Wait for process to finish with timeout
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

        // Get the accumulated output from both tasks
        let stdout = stream_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let exit_code = status.code().unwrap_or(-1);

        if !status.success() {
            // Include both stderr and stdout in the error — Claude CLI sometimes
            // prints errors to stdout in stream-json mode
            let error_output = if stderr.trim().is_empty() {
                stdout.clone()
            } else {
                stderr
            };
            return Err(AgentError::AiCliFailed {
                provider: self.name().to_string(),
                exit_code,
                stderr: error_output,
            });
        }

        let parsed = parse_claude_output(&stdout);

        Ok(AiRunResult {
            output: stdout,
            exit_code,
            tokens_input: parsed.tokens_input,
            tokens_output: parsed.tokens_output,
            model: parsed.model,
            changed_files: None,
            session_id: parsed.session_id,
            is_max_turns_error: parsed.is_max_turns_error,
        })
    }

    async fn continue_conversation(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let is_root = unsafe { libc::geteuid() } == 0;

        // Same shell-free pattern as `run`: argv slots all the way down,
        // never a single `su -c '<string>'` that re-parses user data.
        let mut cmd = if is_root {
            let run_user = if user_exists(SANDBOX_USER) {
                SANDBOX_USER
            } else {
                "nobody"
            };
            let mut c = Command::new("runuser");
            c.arg("-u").arg(run_user).arg("--").arg("claude");
            c
        } else {
            Command::new("claude")
        };
        cmd.arg("--print")
            .arg("--continue")
            .arg(&config.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--max-turns")
            .arg(config.max_turns.to_string())
            .arg("--dangerously-skip-permissions")
            .arg("--verbose");
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        cmd.current_dir(&config.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !config.api_key.is_empty() {
            cmd.env("ANTHROPIC_API_KEY", &config.api_key);
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
                stderr: error_output,
            });
        }

        let parsed = parse_claude_output(&stdout);

        Ok(AiRunResult {
            output: stdout,
            exit_code,
            tokens_input: parsed.tokens_input,
            tokens_output: parsed.tokens_output,
            model: parsed.model,
            changed_files: None,
            session_id: parsed.session_id,
            is_max_turns_error: parsed.is_max_turns_error,
        })
    }
}

/// Parse Claude CLI JSON output for token usage and model information.
/// Claude CLI may emit JSON objects per line (JSON Lines format).
pub struct ParsedClaudeOutput {
    pub tokens_input: Option<i32>,
    pub tokens_output: Option<i32>,
    pub model: Option<String>,
    /// Session ID from the `system/init` event, used for `--resume`.
    pub session_id: Option<String>,
    /// True when the CLI hit the max turns limit without completing.
    pub is_max_turns_error: bool,
}

pub fn parse_claude_output(output: &str) -> ParsedClaudeOutput {
    let mut result = ParsedClaudeOutput {
        tokens_input: None,
        tokens_output: None,
        model: None,
        session_id: None,
        is_max_turns_error: false,
    };

    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            // Extract session_id from system/init event
            if result.session_id.is_none()
                && value.get("type").and_then(|v| v.as_str()) == Some("system")
            {
                result.session_id = value
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            // Look for usage object at top level or nested
            if let Some(usage) = value.get("usage") {
                if result.tokens_input.is_none() {
                    result.tokens_input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32);
                }
                if result.tokens_output.is_none() {
                    result.tokens_output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32);
                }
            }
            if result.model.is_none() {
                result.model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            // Detect max_turns error from the result event
            if value.get("type").and_then(|v| v.as_str()) == Some("result") {
                if value.get("subtype").and_then(|v| v.as_str()) == Some("error_max_turns") {
                    result.is_max_turns_error = true;
                }
                // Also extract usage from the result event's usage field
                if let Some(usage) = value.get("usage") {
                    result.tokens_input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32)
                        .or(result.tokens_input);
                    result.tokens_output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32)
                        .or(result.tokens_output);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_claude_output_with_usage() {
        let output =
            r#"{"model":"claude-3-5-sonnet","usage":{"input_tokens":150,"output_tokens":42}}"#;
        let parsed = parse_claude_output(output);
        assert_eq!(parsed.tokens_input, Some(150));
        assert_eq!(parsed.tokens_output, Some(42));
        assert_eq!(parsed.model.as_deref(), Some("claude-3-5-sonnet"));
    }

    #[test]
    fn test_parse_claude_output_empty() {
        let parsed = parse_claude_output("no json here");
        assert!(parsed.tokens_input.is_none());
        assert!(parsed.tokens_output.is_none());
        assert!(parsed.model.is_none());
        assert!(parsed.session_id.is_none());
    }

    #[test]
    fn test_parse_claude_output_extracts_session_id() {
        let output = r#"{"type":"system","subtype":"init","cwd":"/workspace","session_id":"08290b53-75ae-4be9-985a-d584375bf9e0"}
{"type":"assistant","message":{"model":"claude-sonnet-4-6","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":100,"output_tokens":20}}}"#;
        let parsed = parse_claude_output(output);
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("08290b53-75ae-4be9-985a-d584375bf9e0")
        );
    }

    #[test]
    fn test_parse_claude_output_detects_max_turns_error() {
        let output = r#"{"type":"system","subtype":"init","cwd":"/workspace","session_id":"6dc3ab7b-9272-4b77-9496-1814c75be4e4"}
{"type":"assistant","message":{"model":"claude-sonnet-4-6","content":[{"type":"text","text":"working..."}],"usage":{"input_tokens":100,"output_tokens":20}}}
{"type":"result","subtype":"error_max_turns","duration_ms":84876,"is_error":true,"num_turns":16,"usage":{"input_tokens":500,"output_tokens":200}}"#;
        let parsed = parse_claude_output(output);
        assert!(parsed.is_max_turns_error);
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("6dc3ab7b-9272-4b77-9496-1814c75be4e4")
        );
        // Usage from the result event should be captured
        assert_eq!(parsed.tokens_input, Some(500));
        assert_eq!(parsed.tokens_output, Some(200));
    }

    #[test]
    fn test_parse_claude_output_no_max_turns_error_on_success() {
        let output = r#"{"type":"result","subtype":"success","duration_ms":5000,"is_error":false,"result":"Done","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let parsed = parse_claude_output(output);
        assert!(!parsed.is_max_turns_error);
    }

    #[test]
    fn test_claude_provider_name() {
        let provider = ClaudeCliProvider;
        assert_eq!(provider.name(), "claude_cli");
    }
}
