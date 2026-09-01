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
use crate::sandbox::user::SANDBOX_USER;

const MODEL_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

fn cancellation_safe_command(program: &str) -> Command {
    let mut command = Command::new(program);
    sanitize_command_environment(&mut command);
    copy_environment_variable(&mut command, "ANTHROPIC_API_KEY");
    command.kill_on_drop(true);
    command
}

async fn configure_chat_mcp(cmd: &mut Command, config: &AiRunConfig) -> Result<(), AgentError> {
    // A chat turn must never inherit Claude Code's host shell/filesystem
    // tools, even when no Temps MCP server is configured. Interactive mode
    // additionally needs the two protocol tools that produce question/plan
    // control requests.
    cmd.arg("--tools")
        .arg(if config.permission_bridge.is_some() {
            "AskUserQuestion,ExitPlanMode"
        } else {
            ""
        });

    let Some(server) = &config.mcp_server else {
        return Ok(());
    };
    let path = config.work_dir.join(".temps-chat-mcp.json");
    let contents = serde_json::to_vec(&serde_json::json!({
        "mcpServers": {
            "temps-chat": {
                "type": "http",
                "url": server.url,
                "headers": {
                    "Authorization": format!("Bearer {}", server.authorization_token)
                }
            }
        }
    }))
    .map_err(|error| AgentError::Io(std::io::Error::other(error)))?;
    tokio::fs::write(&path, contents)
        .await
        .map_err(AgentError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(AgentError::Io)?;
    }
    cmd.arg("--mcp-config")
        .arg(path)
        // The bridge itself is already authorization/project scoped, and
        // `temps_write` only stages confirm-gated proposals. Let Claude invoke
        // these MCP tools without an unrelated terminal permission prompt.
        .arg("--allowedTools")
        .arg("mcp__temps-chat__*");
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub effort_levels: Vec<String>,
    pub supports_adaptive_thinking: bool,
    pub supports_auto_mode: bool,
}

/// Turn Claude's resolved model identifier into the versioned label users see
/// in Claude Code. The selectable `value` is intentionally often a moving
/// alias (`sonnet`, `haiku`, `default`), while `resolvedModel` carries the
/// concrete version selected for the authenticated account.
fn resolved_model_display_name(resolved_model: &str) -> Option<String> {
    let resolved_model = resolved_model.strip_prefix("claude-")?;
    let (model_slug, context) = resolved_model
        .strip_suffix(']')
        .and_then(|without_bracket| without_bracket.rsplit_once('['))
        .map_or((resolved_model, None), |(model, context)| {
            (model, Some(context))
        });
    let parts = model_slug.split('-').collect::<Vec<_>>();
    let (family, version_parts) = if parts.first()?.chars().all(|c| c.is_ascii_digit()) {
        let family_index = parts
            .iter()
            .position(|part| !part.chars().all(|c| c.is_ascii_digit()))?;
        (parts[family_index], &parts[..family_index])
    } else {
        let version_end = parts[1..]
            .iter()
            .position(|part| part.len() == 8 && part.chars().all(|c| c.is_ascii_digit()))
            .map(|index| index + 1)
            .unwrap_or(parts.len());
        (parts[0], &parts[1..version_end])
    };
    let version = version_parts
        .iter()
        .take_while(|part| part.chars().all(|c| c.is_ascii_digit()))
        .copied()
        .collect::<Vec<_>>()
        .join(".");
    if version.is_empty() {
        return None;
    }
    let mut family_chars = family.chars();
    let family = family_chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + family_chars.as_str())?;
    let mut label = format!("{family} {version}");
    if let Some(context) = context {
        label.push_str(&format!(" ({} context)", context.to_ascii_uppercase()));
    }
    Some(label)
}

fn model_display_name(model: &serde_json::Value, id: &str) -> String {
    let fallback = model
        .get("displayName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(id);
    let resolved = model
        .get("resolvedModel")
        .and_then(serde_json::Value::as_str)
        .and_then(resolved_model_display_name);
    match (id, resolved) {
        ("default", Some(resolved)) => format!("Default · {resolved}"),
        (_, Some(resolved)) => resolved,
        (_, None) => fallback.to_string(),
    }
}

fn parse_model_initialize_response(value: &serde_json::Value) -> Vec<ClaudeModelInfo> {
    value
        .pointer("/response/response/models")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("value")?.as_str()?.to_string();
            Some(ClaudeModelInfo {
                name: model_display_name(model, &id),
                description: model
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                effort_levels: model
                    .get("supportedEffortLevels")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect(),
                supports_adaptive_thinking: model
                    .get("supportsAdaptiveThinking")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                supports_auto_mode: model
                    .get("supportsAutoMode")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                id,
            })
        })
        .collect()
}

/// Query Claude Code's SDK initialization protocol. This is the same
/// account-aware source used by SDK clients and reflects organization policy,
/// aliases, context-window variants, and model-specific effort capabilities.
pub async fn discover_models() -> Vec<ClaudeModelInfo> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let probe = async {
        let mut command = cancellation_safe_command("claude");
        command
            .args([
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                // Capability discovery must not execute the operator's user,
                // project, or local SessionStart hooks. Managed account policy
                // and the authenticated model list remain available.
                "--setting-sources",
                "",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        stdin
            .write_all(
                b"{\"request_id\":\"temps-models\",\"type\":\"control_request\",\"request\":{\"subtype\":\"initialize\"}}\n",
            )
            .await
            .ok()?;
        stdin.shutdown().await.ok()?;

        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value
                .pointer("/response/request_id")
                .and_then(serde_json::Value::as_str)
                == Some("temps-models")
            {
                return Some(parse_model_initialize_response(&value));
            }
        }
        None
    };

    tokio::time::timeout(MODEL_DISCOVERY_TIMEOUT, probe)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn apply_thinking_args(cmd: &mut Command, thinking_level: Option<&str>) {
    match thinking_level {
        None | Some("default") => {}
        Some("off") => {
            // Claude rejects xhigh when thinking is disabled. Explicitly cap
            // effort so a host-level xhigh default cannot make the advertised
            // Off option fail at runtime.
            cmd.arg("--effort")
                .arg("high")
                .arg("--settings")
                .arg(r#"{"alwaysThinkingEnabled":false}"#);
        }
        Some("ultracode") => {
            cmd.arg("--effort")
                .arg("xhigh")
                .arg("--settings")
                .arg(r#"{"ultracode":true}"#);
        }
        Some(effort) => {
            cmd.arg("--effort").arg(effort);
        }
    }
}

fn apply_permission_args(cmd: &mut Command, permission_mode: Option<&str>) {
    let cli_mode = match permission_mode {
        Some("full-access") => "bypassPermissions",
        Some("accept-edits") => "acceptEdits",
        Some("plan") => "plan",
        _ => "default",
    };
    cmd.arg("--permission-mode").arg(cli_mode);
}

#[async_trait]
impl AiCliProvider for ClaudeCliProvider {
    fn name(&self) -> &str {
        "claude_cli"
    }

    async fn check_installed(&self) -> bool {
        cancellation_safe_command("claude")
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
        let version_output = cancellation_safe_command("claude")
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
        let auth_output = cancellation_safe_command("claude")
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

    async fn discover_model_capabilities(&self) -> Vec<super::AiCliModelCapability> {
        discover_models()
            .await
            .into_iter()
            .map(|model| {
                let has_reasoning =
                    !model.effort_levels.is_empty() || model.supports_adaptive_thinking;
                let mut reasoning_options = if has_reasoning {
                    vec!["off".to_string()]
                } else {
                    Vec::new()
                };
                reasoning_options.extend(model.effort_levels.iter().cloned());
                if model.supports_adaptive_thinking {
                    reasoning_options.push("auto".to_string());
                }
                let default_reasoning_option = model
                    .effort_levels
                    .iter()
                    .find(|effort| effort.as_str() == "medium")
                    .or_else(|| model.effort_levels.first())
                    .cloned();
                super::AiCliModelCapability {
                    id: model.id,
                    name: model.name,
                    reasoning_options,
                    default_reasoning_option,
                }
            })
            .collect()
    }

    fn extract_assistant_text(&self, line: &str) -> Option<String> {
        extract_assistant_text(line)
    }

    fn extract_partial_text(&self, line: &str) -> Option<String> {
        extract_partial_text(line)
    }

    fn dropped_tool_use_name(&self, line: &str) -> Option<String> {
        dropped_tool_use_name(line)
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
            let mut c = cancellation_safe_command("runuser");
            c.arg("-u").arg(run_user).arg("--").arg("claude");
            c
        } else {
            cancellation_safe_command("claude")
        };
        cmd.arg("--print")
            .arg(&config.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            // Without this, `stream-json` only emits one `assistant` event per
            // full turn — the whole reply arrives at once instead of token by
            // token, however early `on_event` is called per NDJSON line. This
            // adds `content_block_delta` lines as the model actually generates
            // text; see `extract_partial_text`.
            .arg("--include-partial-messages");
        apply_permission_args(&mut cmd, config.permission_mode.as_deref());
        apply_thinking_args(&mut cmd, config.thinking_level.as_deref());
        // max_turns <= 0 means "no limit": skip the flag entirely so the
        // CLI uses its own (effectively unbounded) default. The timeout +
        // daily-budget caps still bound the run, so this isn't a free path
        // to infinite cost.
        if config.max_turns > 0 {
            cmd.arg("--max-turns").arg(config.max_turns.to_string());
        }
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        configure_chat_mcp(&mut cmd, &config).await?;
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
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &error_output),
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

    async fn run_turn(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        if config.permission_bridge.is_some() {
            self.run_interactive(config).await
        } else {
            self.run(config).await
        }
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
            let mut c = cancellation_safe_command("runuser");
            c.arg("-u").arg(run_user).arg("--").arg("claude");
            c
        } else {
            cancellation_safe_command("claude")
        };
        cmd.arg("--print")
            .arg("--continue")
            .arg(&config.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            // See the first invocation in `run` — streams deltas instead of
            // buffering the whole reply until the turn ends.
            .arg("--include-partial-messages");
        apply_permission_args(&mut cmd, config.permission_mode.as_deref());
        apply_thinking_args(&mut cmd, config.thinking_level.as_deref());
        // See first invocation: max_turns <= 0 disables the cap entirely.
        if config.max_turns > 0 {
            cmd.arg("--max-turns").arg(config.max_turns.to_string());
        }
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        configure_chat_mcp(&mut cmd, &config).await?;
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
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &error_output),
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

impl ClaudeCliProvider {
    /// Run Claude CLI in interactive (bidirectional) mode, using the
    /// `--permission-prompt-tool stdio` protocol (ADR-038 Phase 2).
    ///
    /// Unlike [`AiCliProvider::run`] — which passes the prompt as a positional
    /// argv, fully bypasses permissions via `--dangerously-skip-permissions`,
    /// and closes stdin immediately — this function:
    ///
    /// * Spawns Claude with `--input-format stream-json --output-format stream-json
    ///   --permission-prompt-tool stdio --verbose` (no `--dangerously-skip-permissions`,
    ///   no `--permission-mode manual` — manual mode hides `AskUserQuestion`/`ExitPlanMode`
    ///   from the tool list, confirmed empirically).
    /// * Writes the initial user message as a single NDJSON line to stdin, then
    ///   **keeps stdin alive inside the stream_task** for the whole subprocess lifetime
    ///   so that `control_response` lines can be written back without restarting.
    /// * Reads stdout line-by-line, reusing [`extract_assistant_text`] and
    ///   [`dropped_tool_use_name`] for text / tool-use lines.
    /// * For `control_request` lines: when a [`super::PermissionBridge`] is present,
    ///   registers a pending permission in the in-process registry, emits the
    ///   appropriate SSE event, awaits the human decision (10-minute timeout), and
    ///   writes the resulting `control_response` back on stdin.  When no bridge is
    ///   present (or timeout fires), falls back to an auto-deny so the CLI is never
    ///   hung indefinitely.
    /// * Returns the same [`AiRunResult`] shape as `run`/`continue_conversation`,
    ///   with `session_id` populated from the `result` or `system/init` event.
    pub async fn run_interactive(
        &self,
        config: super::AiRunConfig,
    ) -> Result<super::AiRunResult, crate::error::AgentError> {
        use std::time::Duration;
        use temps_ai::streaming::{PermissionKind, PermissionRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let is_root = unsafe { libc::geteuid() } == 0;

        // Build the command using the same shell-free pattern as `run` — argv
        // slots all the way to execve(), user prompt never touches a shell.
        let mut cmd = if is_root {
            let run_user = if user_exists(SANDBOX_USER) {
                SANDBOX_USER
            } else {
                "nobody"
            };
            let mut c = cancellation_safe_command("runuser");
            c.arg("-u").arg(run_user).arg("--").arg("claude");
            c
        } else {
            cancellation_safe_command("claude")
        };

        // Interactive protocol flags.
        // IMPORTANT: no --dangerously-skip-permissions and no --permission-mode
        // manual.  `manual` hides AskUserQuestion/ExitPlanMode from the tool
        // list entirely — empirically confirmed with claude 2.1.226.
        cmd.arg("--print")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--permission-prompt-tool")
            .arg("stdio")
            .arg("--verbose")
            // See `run` — streams deltas instead of buffering the whole
            // reply until the turn ends.
            .arg("--include-partial-messages");

        // Interactive turns must honor the same user-selected runtime options
        // as one-shot/continued turns. Omitting these flags caused Claude to
        // inherit host defaults (for example xhigh) while the UI displayed a
        // different selection such as Medium.
        apply_permission_args(&mut cmd, config.permission_mode.as_deref());
        apply_thinking_args(&mut cmd, config.thinking_level.as_deref());

        if config.max_turns > 0 {
            cmd.arg("--max-turns").arg(config.max_turns.to_string());
        }
        if let Some(m) = config.model.as_deref() {
            if !m.is_empty() {
                cmd.arg("--model").arg(m);
            }
        }
        // Resume a previous session when the conversation has a stored session id.
        // `--resume <uuid>` tells the CLI to load its saved session state so the
        // model has the full prior context without re-sending the history in the
        // initial message.
        if let Some(ref session_id) = config.resume_session_id {
            cmd.arg("--resume").arg(session_id);
        }

        configure_chat_mcp(&mut cmd, &config).await?;

        cmd.current_dir(&config.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !config.api_key.is_empty() {
            cmd.env("ANTHROPIC_API_KEY", &config.api_key);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                crate::error::AgentError::AiCliNotInstalled {
                    provider: self.name().to_string(),
                }
            } else {
                crate::error::AgentError::Io(e)
            }
        })?;

        // Take the stdin handle. It is moved into the stream_task so that
        // control_response lines can be written back during the read loop.
        // The task drops it when reading ends (stdout closed = process exited).
        let mut stdin = child.stdin.take().ok_or_else(|| {
            crate::error::AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "claude interactive stdin was not piped",
            ))
        })?;

        // Write the initial user turn as a single NDJSON line.
        let user_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": &config.prompt}]
            }
        });
        let msg_json =
            serde_json::to_string(&user_msg).map_err(|e| crate::error::AgentError::Validation {
                message: format!(
                    "failed to serialize interactive user message for claude: {}",
                    e
                ),
            })?;
        stdin.write_all(msg_json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        let stdout_handle = child.stdout.take().ok_or_else(|| {
            crate::error::AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "claude interactive stdout was not piped",
            ))
        })?;
        let stderr_handle = child.stderr.take().ok_or_else(|| {
            crate::error::AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "claude interactive stderr was not piped",
            ))
        })?;
        let on_event = config.on_event.clone();
        let permission_bridge = config.permission_bridge.clone();
        let provider_name = self.name().to_string();

        // The stream_task owns stdin for its full lifetime:
        //   * It writes control_response lines back on permission approvals.
        //   * When stdout closes (process exited/killed), the loop ends and
        //     stdin is dropped, which is the same timing as the previous
        //     explicit `drop(stdin)` after `child.wait()`.
        let stream_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout_handle);
            let mut lines = reader.lines();
            let mut all_output = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                all_output.push_str(&line);
                all_output.push('\n');

                // Detect control_request frames.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    // The terminal `result` event means the CLI has finished
                    // this turn. In `--input-format stream-json` continuous
                    // mode, the process does NOT exit on its own afterward —
                    // it idles, waiting for either another turn on stdin or
                    // stdin EOF. Since we hold stdin open for the whole
                    // subprocess lifetime, nothing would ever signal it to
                    // exit: `next_line()` below would block forever (no more
                    // stdout, but no EOF either) and the outer `child.wait()`
                    // would hang until `config.timeout` fires. Break here so
                    // the loop's `drop(stdin)` below closes stdin (EOF),
                    // which lets the CLI exit cleanly and `child.wait()`
                    // return promptly instead of hanging for the full
                    // timeout on every turn.
                    if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                        if let Some(ref cb) = on_event {
                            cb(line.clone()).await;
                        }
                        break;
                    }
                    if v.get("type").and_then(|t| t.as_str()) == Some("control_request") {
                        let request_id = v
                            .get("request_id")
                            .and_then(|n| n.as_str())
                            .unwrap_or("<unknown>")
                            .to_string();
                        let tool_name = v
                            .get("request")
                            .and_then(|r| r.get("tool_name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("<unknown>")
                            .to_string();
                        // `subtype` is retained here for protocol completeness
                        // (the frame always carries it) but kind is now derived
                        // from `tool_name` instead — see the bridge block below.
                        let _subtype = v
                            .get("request")
                            .and_then(|r| r.get("subtype"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        let input = v
                            .get("request")
                            .and_then(|r| r.get("input"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        if let Some(bridge) = &permission_bridge {
                            if !matches!(tool_name.as_str(), "AskUserQuestion" | "ExitPlanMode") {
                                tracing::warn!(
                                    provider = %provider_name,
                                    request_id = %request_id,
                                    tool_name = %tool_name,
                                    "denying native Claude tool outside the chat allowlist"
                                );
                                let response_json = build_deny_response(&request_id);
                                if stdin.write_all(response_json.as_bytes()).await.is_err()
                                    || stdin.write_all(b"\n").await.is_err()
                                    || stdin.flush().await.is_err()
                                {
                                    tracing::warn!(
                                        provider = %provider_name,
                                        request_id = %request_id,
                                        "failed to write native-tool denial to claude stdin"
                                    );
                                }
                                continue;
                            }
                            // Derive the permission kind from `tool_name`, not
                            // `subtype`.  The Claude CLI uses `subtype` only for
                            // its own internal flow control; the human-visible
                            // distinction lives in the tool's name:
                            //   • "AskUserQuestion"  → the model is asking a
                            //     direct question, not requesting a tool
                            //   • "ExitPlanMode"     → the model is proposing
                            //     a plan and asking whether to proceed
                            //   • everything else    → generic tool approval
                            let kind = match tool_name.as_str() {
                                "AskUserQuestion" => PermissionKind::Question,
                                "ExitPlanMode" => PermissionKind::PlanApproval,
                                _ => PermissionKind::ToolApproval,
                            };
                            let perm_request = PermissionRequest {
                                id: request_id.clone(),
                                kind,
                                tool_name: tool_name.clone(),
                                input,
                            };

                            // Register in registry and await human decision.
                            // The bridge callback inserts the sender into the
                            // in-process registry and emits the SSE event.
                            let rx = (bridge.on_permission_request)(perm_request);

                            // 10-minute timeout — long enough for a human to
                            // notice, act on the card, and move on.
                            const PERMISSION_TIMEOUT: Duration = Duration::from_secs(600);
                            let decision = tokio::time::timeout(PERMISSION_TIMEOUT, rx).await;

                            let response_json = match decision {
                                Ok(Ok(d)) => build_control_response(&request_id, &v, d),
                                Ok(Err(_)) => {
                                    // oneshot sender was dropped — turn ended.
                                    tracing::warn!(
                                        provider = %provider_name,
                                        request_id = %request_id,
                                        "permission registry entry dropped before \
                                         resolution; auto-denying"
                                    );
                                    build_deny_response(&request_id)
                                }
                                Err(_) => {
                                    // Human didn't respond within 10 minutes.
                                    tracing::warn!(
                                        provider = %provider_name,
                                        request_id = %request_id,
                                        tool_name = %tool_name,
                                        "permission request timed out after 600s; \
                                         auto-denying so the CLI is not hung indefinitely"
                                    );
                                    build_deny_response(&request_id)
                                }
                            };

                            // Write the control_response back. A write error
                            // means the process already exited — log and
                            // continue so the task finishes cleanly.
                            if stdin.write_all(response_json.as_bytes()).await.is_err()
                                || stdin.write_all(b"\n").await.is_err()
                                || stdin.flush().await.is_err()
                            {
                                tracing::warn!(
                                    provider = %provider_name,
                                    request_id = %request_id,
                                    "failed to write control_response to claude stdin; \
                                     process may have already exited"
                                );
                            }
                        } else {
                            // No bridge (milestone 2 fallback): warn and skip.
                            tracing::warn!(
                                provider = %provider_name,
                                tool_name = %tool_name,
                                request_id = %request_id,
                                "claude interactive control_request received but no \
                                 permission bridge is configured; ignoring (see ADR-038)"
                            );
                        }

                        if let Some(ref cb) = on_event {
                            cb(line).await;
                        }
                        continue;
                    }
                }

                if let Some(ref cb) = on_event {
                    cb(line).await;
                }
            }

            // Explicit drop for documentation: stdin is held through the read
            // loop so the channel stays open for control_responses. When
            // reading ends (stdout EOF = process exited), we no longer need it.
            drop(stdin);

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
            Ok(Err(e)) => return Err(crate::error::AgentError::Io(e)),
            Err(_) => {
                let _ = child.kill().await;
                // TODO(LOW): `stream_task` and `stderr_task` are orphaned here.
                // Killing the child closes its stdout/stderr handles so both
                // tasks will drain and self-terminate quickly, but they are not
                // explicitly joined or aborted.  When migrating to a
                // JoinSet-based design, abort both tasks here and then join
                // them before returning so the runtime doesn't carry ghost
                // tasks between requests.
                return Err(crate::error::AgentError::AiCliTimeout {
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
            return Err(crate::error::AgentError::AiCliFailed {
                provider: self.name().to_string(),
                exit_code,
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &error_output),
            });
        }

        let parsed = parse_claude_output(&stdout);

        Ok(super::AiRunResult {
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

/// Build a `control_response` JSON line for an auto-deny (timeout or bridge
/// dropped).  The CLI unblocks with "permission denied" and continues (it may
/// error if the tool was required, but it will never hang indefinitely).
fn build_deny_response(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "deny",
                "message": "Permission denied (no response received)"
            }
        }
    })
    .to_string()
}

/// Build the appropriate `control_response` JSON line for a given
/// [`PermissionDecision`].  Wire format confirmed empirically (ADR-038):
///
/// * Allow: `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":{…}}}}`
/// * Deny:  same but `"behavior":"deny","message":"<reason>"`
///
/// `original_request` is the parsed `control_request` frame — used to echo
/// back the input in the `updatedInput` field of allow responses.
fn build_control_response(
    request_id: &str,
    original_request: &serde_json::Value,
    decision: temps_ai::streaming::PermissionDecision,
) -> String {
    use temps_ai::streaming::PermissionDecision;
    let original_input = original_request
        .get("request")
        .and_then(|r| r.get("input"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let inner_response = match decision {
        PermissionDecision::AllowTool => {
            serde_json::json!({
                "behavior": "allow",
                "updatedInput": original_input
            })
        }
        PermissionDecision::DenyTool { reason } => {
            serde_json::json!({
                "behavior": "deny",
                "message": reason.unwrap_or_else(|| "Permission denied".to_string())
            })
        }
        PermissionDecision::AnswerQuestion { answers } => {
            // AskUserQuestion: echo the original input and merge the answers
            // map so the model receives its questions answered.  Shape
            // confirmed with the live CLI (ADR-038 spike).
            let mut updated = original_input.clone();
            if let serde_json::Value::Object(ref mut map) = updated {
                map.insert("answers".to_string(), answers);
            }
            serde_json::json!({
                "behavior": "allow",
                "updatedInput": updated
            })
        }
        PermissionDecision::ApprovePlan => {
            serde_json::json!({
                "behavior": "allow",
                "updatedInput": {}
            })
        }
        PermissionDecision::RejectPlan { feedback } => {
            serde_json::json!({
                "behavior": "deny",
                "message": feedback.unwrap_or_else(|| "Plan rejected".to_string())
            })
        }
    };

    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": inner_response
        }
    })
    .to_string()
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
                // Extract session_id from result event as a fallback when the
                // system/init event was absent (e.g. in `--input-format stream-json`
                // interactive mode where the init event may carry session_id in a
                // different field). This is the session_id the CLI's own `--resume`
                // flag accepts in a subsequent invocation.
                if result.session_id.is_none() {
                    result.session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }
    }

    result
}

/// Extract assistant text from one line of Claude CLI's `stream-json`
/// output. Only `type":"assistant"` events carry user-facing text (a
/// `message.content[]` array of blocks; only `type":"text"` blocks are
/// prose — tool-use blocks are skipped). Every other event type (`system`,
/// `rate_limit_event`, `result`, hook events, …) returns `None`.
///
/// Deliberately ignores the terminal `type":"result"` event's own `result`
/// field even though it also carries the final answer: in `--print` mode
/// with a single turn, the `assistant` event already carries that exact
/// text, so also emitting the `result` event's copy would duplicate it in a
/// stream of forwarded chunks.
pub fn extract_assistant_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let content = value.get("message")?.get("content")?.as_array()?;
    let text: String = content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
        .collect();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Extract one incremental text delta from a `--include-partial-messages`
/// line (`type":"stream_event"`, `event.type":"content_block_delta"`,
/// `event.delta.type":"text_delta"`) — the actual token-by-token stream, sent
/// as the model generates rather than once the whole turn completes. Returns
/// `None` for every other line, including the consolidated `type":"assistant"`
/// event at the end of the turn: with partial messages on, that event's text
/// is always a duplicate of what these deltas already delivered (see
/// [`extract_assistant_text`]'s doc comment on why it's ignored there when a
/// caller uses both).
pub fn extract_partial_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("stream_event") {
        return None;
    }
    let event = value.get("event")?;
    if event.get("type").and_then(|v| v.as_str()) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(|v| v.as_str()) != Some("text_delta") {
        return None;
    }
    let text = delta.get("text").and_then(|v| v.as_str())?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Name the tool a `tool_use` content block invoked, when `line` is an
/// `assistant` event whose content is a tool call rather than (or alongside)
/// text — e.g. `AskUserQuestion`, `ExitPlanMode`, `Bash`. Returns `None` for
/// any other event (see [`extract_assistant_text`]).
///
/// Used only for diagnostic logging (ADR-038): CLI-chat cannot bridge these
/// calls back to the user, so the turn silently drops them. Logging the tool
/// name (never `input`, which may carry user data) lets an operator see in
/// server logs why a turn ended with an empty response instead of the event
/// vanishing without a trace.
pub fn dropped_tool_use_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let content = value.get("message")?.get("content")?.as_array()?;
    content
        .iter()
        .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .and_then(|block| block.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_account_aware_models_and_effort_capabilities() {
        let response = serde_json::json!({
            "type": "control_response",
            "response": {
                "request_id": "temps-models",
                "response": {
                    "models": [
                        {
                            "value": "default",
                            "resolvedModel": "claude-opus-5[1m]",
                            "displayName": "Default (recommended)",
                            "description": "Opus 5 with 1M context · Best for everyday, complex tasks",
                            "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]
                        },
                        {
                            "value": "opus[1m]",
                            "resolvedModel": "claude-opus-5[1m]",
                            "displayName": "Opus (1M context)",
                            "description": "Opus 5 with 1M context",
                            "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"],
                            "supportsAdaptiveThinking": true,
                            "supportsAutoMode": true
                        },
                        {
                            "value": "haiku",
                            "resolvedModel": "claude-haiku-4-5-20251001",
                            "displayName": "Haiku",
                            "description": "Fast",
                            "supportedEffortLevels": []
                        }
                    ]
                }
            }
        });

        let models = parse_model_initialize_response(&response);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "default");
        assert_eq!(models[0].name, "Default · Opus 5 (1M context)");
        assert_eq!(models[1].id, "opus[1m]");
        assert_eq!(models[1].name, "Opus 5 (1M context)");
        assert_eq!(
            models[1].effort_levels,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(models[1].supports_adaptive_thinking);
        assert!(models[1].supports_auto_mode);
        assert_eq!(models[2].name, "Haiku 4.5");
        assert!(models[2].effort_levels.is_empty());
    }

    #[test]
    fn formats_both_current_and_legacy_resolved_claude_model_ids() {
        assert_eq!(
            resolved_model_display_name("claude-sonnet-5").as_deref(),
            Some("Sonnet 5")
        );
        assert_eq!(
            resolved_model_display_name("claude-3-7-sonnet-20250219").as_deref(),
            Some("Sonnet 3.7")
        );
    }

    fn thinking_args(level: &str) -> Vec<String> {
        let mut command = cancellation_safe_command("claude");
        apply_thinking_args(&mut command, Some(level));
        command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn claude_extended_thinking_modes_map_to_supported_cli_arguments() {
        assert_eq!(thinking_args("max"), ["--effort", "max"]);
        assert_eq!(thinking_args("xhigh"), ["--effort", "xhigh"]);
        assert_eq!(
            thinking_args("off"),
            [
                "--effort",
                "high",
                "--settings",
                r#"{"alwaysThinkingEnabled":false}"#
            ]
        );
        assert_eq!(
            thinking_args("ultracode"),
            ["--effort", "xhigh", "--settings", r#"{"ultracode":true}"#,]
        );
    }

    #[test]
    fn claude_permission_modes_map_to_cli_arguments() {
        for (mode, expected) in [
            ("default", "default"),
            ("accept-edits", "acceptEdits"),
            ("plan", "plan"),
            ("full-access", "bypassPermissions"),
        ] {
            let mut command = cancellation_safe_command("claude");
            apply_permission_args(&mut command, Some(mode));
            let args = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(args, ["--permission-mode", expected]);
        }
    }

    #[tokio::test]
    async fn claude_registers_ephemeral_chat_mcp_config() {
        let scratch = tempfile::tempdir().expect("create scratch directory");
        let config = super::super::AiRunConfig {
            work_dir: scratch.path().to_owned(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: Some("default".to_string()),
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: Some(super::super::McpServerConfig {
                url: "http://127.0.0.1:32123/mcp/turn".to_string(),
                authorization_token: "ephemeral-test-token".to_string(),
            }),
        };
        let mut command = cancellation_safe_command("claude");
        configure_chat_mcp(&mut command, &config)
            .await
            .expect("write MCP config");
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair[0] == "--mcp-config"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "mcp__temps-chat__*"]));
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        let json: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(scratch.path().join(".temps-chat-mcp.json"))
                .await
                .expect("read MCP config"),
        )
        .expect("parse MCP config");
        assert_eq!(
            json["mcpServers"]["temps-chat"]["headers"]["Authorization"],
            "Bearer ephemeral-test-token"
        );
    }

    #[tokio::test]
    async fn interactive_claude_only_exposes_protocol_and_scoped_mcp_tools() {
        let scratch = tempfile::tempdir().expect("create scratch directory");
        let config = super::super::AiRunConfig {
            work_dir: scratch.path().to_owned(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: Some("default".to_string()),
            on_event: None,
            permission_bridge: Some(std::sync::Arc::new(super::super::PermissionBridge {
                on_permission_request: std::sync::Arc::new(|_| {
                    let (_tx, rx) = tokio::sync::oneshot::channel();
                    rx
                }),
            })),
            resume_session_id: None,
            mcp_server: Some(super::super::McpServerConfig {
                url: "http://127.0.0.1:32123/mcp/turn".to_string(),
                authorization_token: "ephemeral-test-token".to_string(),
            }),
        };
        let mut command = cancellation_safe_command("claude");
        configure_chat_mcp(&mut command, &config)
            .await
            .expect("write MCP config");
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--tools", "AskUserQuestion,ExitPlanMode"]));
    }

    #[tokio::test]
    async fn noninteractive_claude_denies_native_tools_without_mcp() {
        let scratch = tempfile::tempdir().expect("create scratch directory");
        let config = super::super::AiRunConfig {
            work_dir: scratch.path().to_owned(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: Some("default".to_string()),
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };
        let mut command = cancellation_safe_command("claude");
        configure_chat_mcp(&mut command, &config)
            .await
            .expect("configure tool restrictions");
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(!args.iter().any(|arg| arg == "--mcp-config"));
        assert!(!args.iter().any(|arg| arg == "--allowedTools"));
    }

    #[tokio::test]
    async fn interactive_claude_only_exposes_protocol_tools_without_mcp() {
        let scratch = tempfile::tempdir().expect("create scratch directory");
        let config = super::super::AiRunConfig {
            work_dir: scratch.path().to_owned(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: None,
            thinking_level: None,
            permission_mode: Some("default".to_string()),
            on_event: None,
            permission_bridge: Some(std::sync::Arc::new(super::super::PermissionBridge {
                on_permission_request: std::sync::Arc::new(|_| {
                    let (_tx, rx) = tokio::sync::oneshot::channel();
                    rx
                }),
            })),
            resume_session_id: None,
            mcp_server: None,
        };
        let mut command = cancellation_safe_command("claude");
        configure_chat_mcp(&mut command, &config)
            .await
            .expect("configure tool restrictions");
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--tools", "AskUserQuestion,ExitPlanMode"]));
        assert!(!args.iter().any(|arg| arg == "--mcp-config"));
        assert!(!args.iter().any(|arg| arg == "--allowedTools"));
    }

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

    #[test]
    fn test_extract_assistant_text_from_assistant_event() {
        let line = r#"{"type":"assistant","message":{"model":"claude-sonnet-5","content":[{"type":"text","text":"4"}]}}"#;
        assert_eq!(extract_assistant_text(line).as_deref(), Some("4"));
    }

    #[test]
    fn test_extract_assistant_text_ignores_non_assistant_events() {
        let system =
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup"}"#;
        let result = r#"{"type":"result","subtype":"success","result":"4","is_error":false}"#;
        let rate_limit = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#;
        assert_eq!(extract_assistant_text(system), None);
        assert_eq!(extract_assistant_text(result), None);
        assert_eq!(extract_assistant_text(rate_limit), None);
    }

    #[test]
    fn test_extract_assistant_text_ignores_tool_use_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"bash","input":{}}]}}"#;
        assert_eq!(extract_assistant_text(line), None);
    }

    #[test]
    fn test_extract_assistant_text_concatenates_multiple_text_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello, "},{"type":"text","text":"world!"}]}}"#;
        assert_eq!(
            extract_assistant_text(line).as_deref(),
            Some("Hello, world!")
        );
    }

    #[test]
    fn test_extract_assistant_text_rejects_non_json_line() {
        assert_eq!(extract_assistant_text("not json"), None);
    }

    #[test]
    fn test_extract_partial_text_from_content_block_delta() {
        // Real shape captured from `claude --print ... --include-partial-messages`.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ban"}}}"#;
        assert_eq!(extract_partial_text(line).as_deref(), Some("ban"));
    }

    #[test]
    fn test_extract_partial_text_ignores_non_stream_event_lines() {
        let assistant =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"banana"}]}}"#;
        let system = r#"{"type":"system","subtype":"init"}"#;
        assert_eq!(extract_partial_text(assistant), None);
        assert_eq!(extract_partial_text(system), None);
    }

    #[test]
    fn test_extract_partial_text_ignores_other_stream_event_types() {
        // message_start, content_block_start, content_block_stop, message_delta,
        // message_stop — none carry a text delta.
        let message_start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-5"}}}"#;
        let block_start = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
        let block_stop =
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#;
        assert_eq!(extract_partial_text(message_start), None);
        assert_eq!(extract_partial_text(block_start), None);
        assert_eq!(extract_partial_text(block_stop), None);
    }

    #[test]
    fn test_extract_partial_text_ignores_tool_use_input_json_delta() {
        // A tool-call argument delta carries `delta.type":"input_json_delta"`,
        // not `text_delta` — must not be forwarded as chat prose.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"foo\""}}}"#;
        assert_eq!(extract_partial_text(line), None);
    }

    #[test]
    fn test_extract_partial_text_rejects_non_json_line() {
        assert_eq!(extract_partial_text("not json"), None);
    }

    #[test]
    fn test_dropped_tool_use_name_from_ask_user_question() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"AskUserQuestion","input":{"questions":[]}}]}}"#;
        assert_eq!(
            dropped_tool_use_name(line).as_deref(),
            Some("AskUserQuestion")
        );
    }

    #[test]
    fn test_dropped_tool_use_name_none_for_text_only_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        assert_eq!(dropped_tool_use_name(line), None);
    }

    #[test]
    fn test_dropped_tool_use_name_none_for_non_assistant_events() {
        let system = r#"{"type":"system","subtype":"hook_started"}"#;
        assert_eq!(dropped_tool_use_name(system), None);
    }

    #[test]
    fn test_dropped_tool_use_name_never_leaks_input() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"Bash","input":{"command":"rm -rf /secret"}}]}}"#;
        let name = dropped_tool_use_name(line);
        assert_eq!(name.as_deref(), Some("Bash"));
        assert!(!name.unwrap().contains("rm -rf"));
    }

    // ----- run_interactive NDJSON parsing tests -----

    /// `control_request` lines are emitted by `claude` when a tool needs
    /// interactive approval.  The milestone-2 handler ignores them (logs only),
    /// but the parsing logic must correctly detect them.
    #[test]
    fn test_control_request_line_detected() {
        let line = r#"{"type":"control_request","request_id":"req-abc","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{},"tool_use_id":"tu-1","requires_user_interaction":true}}"#;
        let v: serde_json::Value = serde_json::from_str(line).expect("fixture must parse");
        assert_eq!(
            v.get("type").and_then(|t| t.as_str()),
            Some("control_request"),
            "type field must be 'control_request'"
        );
        let tool_name = v
            .get("request")
            .and_then(|r| r.get("tool_name"))
            .and_then(|n| n.as_str());
        assert_eq!(tool_name, Some("AskUserQuestion"));
        let request_id = v.get("request_id").and_then(|n| n.as_str());
        assert_eq!(request_id, Some("req-abc"));
    }

    /// `control_request` must NOT be mistaken for an `assistant` event by the
    /// existing text/tool-use helpers — both should return `None` for it.
    #[test]
    fn test_control_request_not_matched_by_extract_or_dropped() {
        let line = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}"#;
        assert_eq!(
            extract_assistant_text(line),
            None,
            "extract_assistant_text must ignore control_request"
        );
        assert_eq!(
            dropped_tool_use_name(line),
            None,
            "dropped_tool_use_name must ignore control_request"
        );
    }

    /// `parse_claude_output` must extract `session_id` from a `result` line
    /// when no `system/init` event preceded it (as may occur in
    /// `--input-format stream-json` turns).
    #[test]
    fn test_parse_claude_output_extracts_session_id_from_result_event() {
        let output = r#"{"type":"result","subtype":"success","session_id":"result-session-id-999","duration_ms":1234,"is_error":false,"usage":{"input_tokens":10,"output_tokens":5}}"#;
        let parsed = parse_claude_output(output);
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("result-session-id-999"),
            "session_id must be extracted from result event when system/init is absent"
        );
    }

    /// `system/init` session_id takes precedence over a `result` session_id
    /// when both appear in the same stream (as is typical in full real runs).
    #[test]
    fn test_parse_claude_output_system_session_id_takes_precedence_over_result() {
        let output = r#"{"type":"system","subtype":"init","session_id":"init-sid","cwd":"/tmp"}
{"type":"result","subtype":"success","session_id":"result-sid","is_error":false}"#;
        let parsed = parse_claude_output(output);
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("init-sid"),
            "system/init session_id must take precedence over result session_id"
        );
    }

    // ----- build_control_response / build_deny_response tests -----

    #[test]
    fn test_build_deny_response_structure() {
        let json = build_deny_response("req-abc-123");
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        assert_eq!(v["type"], "control_response");
        let resp = &v["response"];
        assert_eq!(resp["subtype"], "success");
        assert_eq!(resp["request_id"], "req-abc-123");
        assert_eq!(resp["response"]["behavior"], "deny");
        assert!(
            !resp["response"]["message"].is_null(),
            "deny must have message"
        );
    }

    #[test]
    fn test_build_control_response_allow_echoes_input() {
        use temps_ai::streaming::PermissionDecision;
        let original = serde_json::json!({
            "type": "control_request",
            "request_id": "r1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "ls"},
                "tool_use_id": "tu-1",
                "requires_user_interaction": true
            }
        });
        let json = build_control_response("r1", &original, PermissionDecision::AllowTool);
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        assert_eq!(v["type"], "control_response");
        let inner = &v["response"]["response"];
        assert_eq!(inner["behavior"], "allow");
        // updatedInput must echo the original input
        assert_eq!(inner["updatedInput"]["command"], "ls");
    }

    #[test]
    fn test_build_control_response_deny_tool_with_reason() {
        use temps_ai::streaming::PermissionDecision;
        let original =
            serde_json::json!({"type":"control_request","request_id":"r2","request":{"input":{}}});
        let json = build_control_response(
            "r2",
            &original,
            PermissionDecision::DenyTool {
                reason: Some("blocked by policy".to_string()),
            },
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        let inner = &v["response"]["response"];
        assert_eq!(inner["behavior"], "deny");
        assert_eq!(inner["message"], "blocked by policy");
    }

    #[test]
    fn test_build_control_response_answer_question_merges_answers() {
        use temps_ai::streaming::PermissionDecision;
        let original = serde_json::json!({
            "type": "control_request",
            "request_id": "r3",
            "request": {
                "input": {
                    "questions": [{"prompt": "header", "options": [{"label": "Yes"}, {"label": "No"}], "question": "Do you agree?"}]
                }
            }
        });
        let answers = serde_json::json!({"Do you agree?": "Yes"});
        let json = build_control_response(
            "r3",
            &original,
            PermissionDecision::AnswerQuestion {
                answers: answers.clone(),
            },
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        let inner = &v["response"]["response"];
        assert_eq!(inner["behavior"], "allow");
        // answers must be merged into updatedInput
        assert_eq!(inner["updatedInput"]["answers"]["Do you agree?"], "Yes");
        // original questions must still be present
        assert!(!inner["updatedInput"]["questions"].is_null());
    }

    #[test]
    fn test_build_control_response_approve_plan() {
        use temps_ai::streaming::PermissionDecision;
        let original =
            serde_json::json!({"type":"control_request","request_id":"r4","request":{"input":{}}});
        let json = build_control_response("r4", &original, PermissionDecision::ApprovePlan);
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
    }

    #[test]
    fn test_build_control_response_reject_plan_with_feedback() {
        use temps_ai::streaming::PermissionDecision;
        let original =
            serde_json::json!({"type":"control_request","request_id":"r5","request":{"input":{}}});
        let json = build_control_response(
            "r5",
            &original,
            PermissionDecision::RejectPlan {
                feedback: Some("needs revision".to_string()),
            },
        );
        let v: serde_json::Value = serde_json::from_str(&json).expect("must parse");
        let inner = &v["response"]["response"];
        assert_eq!(inner["behavior"], "deny");
        assert_eq!(inner["message"], "needs revision");
    }

    /// Integration test: `run_interactive` with a real permission bridge that
    /// answers the first user question. Skips gracefully if `claude` is
    /// absent/unauthenticated (CI).
    #[tokio::test]
    async fn test_run_interactive_with_permission_bridge_answers_question() {
        use std::sync::Arc;
        use temps_ai::streaming::{PermissionDecision, PermissionRequest};
        use tokio::sync::oneshot;

        let provider = ClaudeCliProvider;
        if !provider.check_installed().await {
            println!("claude CLI not installed — skipping permission bridge integration test");
            return;
        }

        let bridge_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called = bridge_called.clone();
        let bridge = super::super::PermissionBridge {
            on_permission_request: Arc::new(move |req: PermissionRequest| {
                called.store(true, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(req.tool_name, "AskUserQuestion");
                let (tx, rx) = oneshot::channel::<PermissionDecision>();
                let _ = tx.send(PermissionDecision::AnswerQuestion {
                    answers: serde_json::json!({"Which option?": "Alpha"}),
                });
                rx
            }),
        };

        let config = super::super::AiRunConfig {
            work_dir: std::env::temp_dir(),
            prompt: "Use AskUserQuestion exactly once. Ask `Which option?` with Alpha and Beta, then acknowledge the answer."
                .to_string(),
            api_key: String::new(),
            max_turns: 3,
            timeout: std::time::Duration::from_secs(60),
            model: None,
            thinking_level: None,
            permission_mode: None,
            on_event: None,
            permission_bridge: Some(Arc::new(bridge)),
            resume_session_id: None,
            mcp_server: None,
        };

        match provider.run_interactive(config).await {
            Ok(result) => {
                println!(
                    "run_interactive with bridge completed; output snippet: {}",
                    &result.output[..result.output.len().min(300)]
                );
                // The test just asserts the subprocess finished without error.
                // Whether "hello_from_bridge" appears depends on auth/model,
                // so we only check that we got some output back.
                assert!(!result.output.is_empty(), "must return non-empty output");
                assert!(
                    bridge_called.load(std::sync::atomic::Ordering::SeqCst),
                    "real Claude control request must traverse the permission bridge"
                );
            }
            Err(crate::error::AgentError::AiCliNotInstalled { .. })
            | Err(crate::error::AgentError::AiCliFailed { .. })
            | Err(crate::error::AgentError::AiCliTimeout { .. }) => {
                println!("claude CLI not available or unauthenticated in CI — skipping");
            }
            Err(e) => {
                panic!("unexpected error from run_interactive with bridge: {:?}", e);
            }
        }
    }

    /// Integration-style smoke test for `run_interactive`.  Skips gracefully
    /// when the `claude` binary is absent (CI), following the codebase pattern
    /// for Docker-dependent tests — no `#[ignore]`, runtime skip instead.
    #[tokio::test]
    async fn test_run_interactive_skips_or_returns_session_id() {
        let provider = ClaudeCliProvider;
        if !provider.check_installed().await {
            println!("claude CLI not installed — skipping run_interactive subprocess test");
            return;
        }

        let config = super::super::AiRunConfig {
            work_dir: std::env::temp_dir(),
            prompt: "Respond with the single word: hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            // Short timeout so an unauthenticated/hung CLI fails fast in CI
            // rather than blocking for the full interactive-session wait.
            timeout: std::time::Duration::from_secs(15),
            model: None,
            thinking_level: None,
            permission_mode: None,
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: None,
        };

        match provider.run_interactive(config).await {
            Ok(result) => {
                assert!(
                    result.session_id.is_some(),
                    "run_interactive must return a session_id when claude exits successfully; got output: {}",
                    &result.output[..result.output.len().min(500)]
                );
                assert!(
                    !result.output.is_empty(),
                    "run_interactive must return non-empty output"
                );
            }
            // Not installed / failed due to auth / timed out (unauthenticated
            // CLI hangs on login prompt instead of failing fast): all acceptable
            // in CI environments where claude is not authenticated.
            Err(crate::error::AgentError::AiCliNotInstalled { .. }) => {
                println!("claude CLI reported not installed during run_interactive — skipping");
            }
            Err(crate::error::AgentError::AiCliFailed { stderr, .. }) => {
                println!(
                    "claude CLI failed (possibly unauthenticated in CI) — skipping: {}",
                    stderr
                );
            }
            Err(crate::error::AgentError::AiCliTimeout { .. }) => {
                println!(
                    "claude CLI timed out (likely unauthenticated or rate-limited in CI) — skipping"
                );
            }
            Err(e) => {
                panic!("unexpected error from run_interactive: {:?}", e);
            }
        }
    }
}
