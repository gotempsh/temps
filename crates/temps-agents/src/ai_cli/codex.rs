// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use super::host_cli::{self, HostIdentity, ProviderLocation};
use super::login::{
    validate_verification_url, LoginChallenge, LoginControl, LoginError, LoginHandle,
    CHALLENGE_TIMEOUT, CODEX_LOGIN_HOSTS, MAX_SCANNED_OUTPUT_BYTES,
};
use super::{
    copy_environment_variable, AiCliProvider, AiCliStatus, AiRunConfig, AiRunResult,
    NativeToolEvent,
};
use crate::error::AgentError;

const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub struct CodexCliProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModelInfo {
    pub id: String,
    pub name: String,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
}

/// The `codex` command every host-side invocation starts from. Codex has
/// always run as the server's own user, so sign-in, status and turns share
/// that identity and, once an operator has signed in through Temps, the
/// Temps-owned `CODEX_HOME` (see [`host_cli`]).
fn codex_command() -> Command {
    let location = host_cli::active_managed_location("codex_cli");
    let mut command = host_cli::host_command("codex", &HostIdentity::current(), location.as_ref());
    copy_environment_variable(&mut command, "OPENAI_API_KEY");
    command
}

fn apply_runtime_args(
    cmd: &mut Command,
    permission_mode: Option<&str>,
    thinking_level: Option<&str>,
) {
    match permission_mode {
        Some("full-access") => {
            cmd.arg("--dangerously-bypass-approvals-and-sandbox");
        }
        Some("auto-review") => {
            cmd.arg("--sandbox")
                .arg("workspace-write")
                .arg("--config")
                .arg("approval_policy=\"on-request\"")
                .arg("--config")
                .arg("approvals_reviewer=\"auto_review\"");
        }
        _ => {
            // `codex exec` is non-interactive: there is no terminal attached
            // that can answer an MCP approval prompt. Plain `auto` and
            // `approval_policy="never"` both make Codex report scoped MCP
            // calls as "user cancelled". Its auto reviewer is the supported
            // non-interactive path: it approves the scoped read-only call while
            // keeping command execution inside the workspace-write sandbox.
            cmd.arg("--sandbox")
                .arg("workspace-write")
                .arg("--config")
                .arg("approval_policy=\"on-request\"")
                .arg("--config")
                .arg("approvals_reviewer=\"auto_review\"");
        }
    }
    if let Some(effort) = thinking_level.filter(|effort| *effort != "default") {
        cmd.arg("--config")
            .arg(format!("model_reasoning_effort=\"{effort}\""));
    }
}

fn build_run_command(config: &AiRunConfig) -> Command {
    let mut cmd = codex_command();
    cmd.arg("exec")
        .arg(&config.prompt)
        .arg("--json")
        // Chat turns must be governed solely by Temps' normalized prompt and
        // ephemeral MCP bridge. Ambient Codex skills/rules can redefine
        // `temps` as a local binary and divert the model away from the scoped
        // MCP tool. Authentication still comes from CODEX_HOME, and explicit
        // command-line MCP settings below remain active.
        .arg("--ignore-user-config")
        .arg("--ignore-rules")
        .arg("--ephemeral")
        // Agent CLI chat deliberately runs in an empty, isolated scratch
        // directory. Codex otherwise rejects the invocation because that
        // directory is not a trusted Git repository.
        .arg("--skip-git-repo-check");
    apply_runtime_args(
        &mut cmd,
        config.permission_mode.as_deref(),
        config.thinking_level.as_deref(),
    );
    if let Some(model) = config.model.as_deref().filter(|model| !model.is_empty()) {
        cmd.arg("--model").arg(model);
    }
    if let Some(server) = &config.mcp_server {
        cmd.arg("--config")
            .arg(format!("mcp_servers.temps_chat.url=\"{}\"", server.url))
            .arg("--config")
            .arg("mcp_servers.temps_chat.bearer_token_env_var=\"TEMPS_CHAT_MCP_TOKEN\"")
            .env("TEMPS_CHAT_MCP_TOKEN", &server.authorization_token);
    }
    cmd.current_dir(&config.work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !config.api_key.is_empty() {
        cmd.env("OPENAI_API_KEY", &config.api_key);
    }
    cmd
}

fn parse_model_list_response(value: &serde_json::Value) -> Vec<CodexModelInfo> {
    let models = value
        .pointer("/result/data")
        .or_else(|| value.pointer("/result/models"))
        .or_else(|| value.get("result"))
        .and_then(serde_json::Value::as_array);
    let Some(models) = models else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|model| {
            let id = model
                .get("id")
                .or_else(|| model.get("model"))
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let name = model
                .get("displayName")
                .or_else(|| model.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let reasoning_efforts = model
                .get("supportedReasoningEfforts")
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|effort| {
                            effort
                                .as_str()
                                .or_else(|| effort.get("reasoningEffort")?.as_str())
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let default_reasoning_effort = model
                .get("defaultReasoningEffort")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            Some(CodexModelInfo {
                id,
                name,
                reasoning_efforts,
                default_reasoning_effort,
            })
        })
        .collect()
}

fn model_capabilities(models: Vec<CodexModelInfo>) -> Vec<super::AiCliModelCapability> {
    models
        .into_iter()
        .map(|model| super::AiCliModelCapability {
            id: model.id,
            name: model.name,
            reasoning_options: model.reasoning_efforts,
            default_reasoning_option: model.default_reasoning_effort,
        })
        .collect()
}

/// Parse the account-aware `model/list` response emitted by Codex app-server
/// into the provider-neutral model contract used by the workspace picker.
/// Sandbox discovery and host discovery intentionally share this parser.
pub fn parse_model_capabilities_from_app_server_output(
    output: &str,
) -> Vec<super::AiCliModelCapability> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("id").and_then(serde_json::Value::as_i64) == Some(2))
        .map(|value| model_capabilities(parse_model_list_response(&value)))
        .unwrap_or_default()
}

/// Query the installed Codex app-server for its account-specific model list.
/// The child is killed on timeout/drop and no credential contents are read.
pub async fn discover_models() -> Vec<CodexModelInfo> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let probe = async {
        let mut child = codex_command();
        child
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = child.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let requests = concat!(
            "{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"temps\",\"version\":\"1\"}}}\n",
            "{\"method\":\"initialized\",\"params\":{}}\n",
            "{\"id\":2,\"method\":\"model/list\",\"params\":{}}\n"
        );
        stdin.write_all(requests.as_bytes()).await.ok()?;
        stdin.shutdown().await.ok()?;
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let value = serde_json::from_str::<serde_json::Value>(&line).ok()?;
            if value.get("id").and_then(serde_json::Value::as_i64) == Some(2) {
                return Some(parse_model_list_response(&value));
            }
        }
        None
    };
    tokio::time::timeout(STATUS_TIMEOUT, probe)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Probe installation and sign-in state with commands built by `make_command`.
/// `ambient_api_key` controls whether `OPENAI_API_KEY` in the server
/// environment counts as being signed in: it must not when confirming that a
/// subscription sign-in produced a credential.
async fn probe_codex_status(
    make_command: impl Fn() -> Command,
    ambient_api_key: bool,
) -> AiCliStatus {
    let version_output = tokio::time::timeout(
        STATUS_TIMEOUT,
        make_command()
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
                provider: "codex_cli".into(),
                installed: false,
                version: None,
                authenticated: false,
                auth_method: None,
                email: None,
                subscription_type: None,
                setup_hint: Some("Install Codex CLI: npm install -g @openai/codex".into()),
            };
        }
    };

    // `codex login status` is the narrow, redacted readiness probe and
    // normally returns immediately. Do not put `doctor --json` first:
    // doctor also checks unrelated network/MCP state and can take longer
    // than the provider catalog's entire timeout, making a healthy Codex
    // login disappear intermittently. Only use the richer diagnostic as a
    // compatibility fallback when login status cannot identify auth.
    let has_env_key = ambient_api_key && std::env::var("OPENAI_API_KEY").is_ok();
    let login_output = tokio::time::timeout(
        STATUS_TIMEOUT,
        make_command()
            .args(["login", "status"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await;
    let login_auth_method = login_output
        .as_ref()
        .ok()
        .and_then(|result| result.as_ref().ok())
        .filter(|output| output.status.success())
        .and_then(|output| {
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            codex_auth_method(&combined)
        });

    let cli_auth_method = if login_auth_method.is_some() {
        login_auth_method
    } else {
        let doctor_output = tokio::time::timeout(
            STATUS_TIMEOUT,
            make_command()
                .args(["doctor", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;
        doctor_output
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok())
            .and_then(|output| codex_doctor_auth_method(&String::from_utf8_lossy(&output.stdout)))
    };
    let authenticated = has_env_key || cli_auth_method.is_some();
    let auth_method = if has_env_key {
        Some("api_key_env".to_string())
    } else {
        cli_auth_method
    };

    AiCliStatus {
        provider: "codex_cli".into(),
        installed,
        version,
        authenticated,
        auth_method,
        email: None,
        subscription_type: None,
        setup_hint: if authenticated {
            None
        } else {
            Some(
                "Run 'codex login' as the user running Temps, or provide OPENAI_API_KEY to the Temps process."
                    .into(),
            )
        },
    }
}

/// Ask the CLI that owns `location` to use (and therefore refresh) its own
/// credential with one minimal request, so the access token can be
/// re-snapshotted. Output is discarded.
pub async fn refresh_managed_login(
    location: &ProviderLocation,
    timeout: std::time::Duration,
) -> Result<(), LoginError> {
    let mut command = host_cli::host_command("codex", &HostIdentity::current(), Some(location));
    command
        .args([
            "exec",
            "ok",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
        ])
        .current_dir(&location.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| LoginError::Spawn {
        provider_id: "codex_cli".to_string(),
        reason: error.to_string(),
    })?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(LoginError::CliRejected {
            provider_id: "codex_cli".to_string(),
            exit_code: status.code().unwrap_or(-1),
        }),
        Ok(Err(error)) => Err(LoginError::Io {
            provider_id: "codex_cli".to_string(),
            operation: "waiting for the credential refresh".to_string(),
            reason: error.to_string(),
        }),
        Err(_) => {
            let _ = child.kill().await;
            Err(LoginError::Timeout {
                provider_id: "codex_cli".to_string(),
                timeout_secs: timeout.as_secs(),
            })
        }
    }
}

/// How to launch the device-code sign-in; `program` is injectable for tests.
pub struct CodexLoginOptions {
    pub program: std::ffi::OsString,
    pub identity: HostIdentity,
    pub location: ProviderLocation,
    pub challenge_timeout: std::time::Duration,
}

/// What one JSON-RPC line from `codex app-server` means for the sign-in.
#[derive(Debug, PartialEq, Eq)]
enum CodexLoginMessage {
    /// Response to `account/login/start` carrying the device code.
    Challenge {
        login_id: Option<String>,
        verification_url: String,
        user_code: String,
    },
    /// The login request was refused with a JSON-RPC error code.
    Refused {
        code: i64,
    },
    /// `account/login/completed`.
    Completed {
        login_id: Option<String>,
        success: bool,
    },
    Other,
}

const LOGIN_START_REQUEST_ID: i64 = 2;

fn parse_codex_login_message(line: &str) -> CodexLoginMessage {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return CodexLoginMessage::Other;
    };
    if value.get("id").and_then(serde_json::Value::as_i64) == Some(LOGIN_START_REQUEST_ID) {
        if let Some(error) = value.get("error") {
            return CodexLoginMessage::Refused {
                code: error
                    .get("code")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0),
            };
        }
        let result = value.get("result");
        let text = |camel: &str, snake: &str| {
            result
                .and_then(|result| result.get(camel).or_else(|| result.get(snake)))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        if let (Some(verification_url), Some(user_code)) = (
            text("verificationUrl", "verification_url"),
            text("userCode", "user_code"),
        ) {
            return CodexLoginMessage::Challenge {
                login_id: text("loginId", "login_id"),
                verification_url,
                user_code,
            };
        }
        return CodexLoginMessage::Other;
    }
    if value.get("method").and_then(serde_json::Value::as_str) == Some("account/login/completed") {
        let params = value.get("params");
        return CodexLoginMessage::Completed {
            login_id: params
                .and_then(|params| params.get("loginId").or_else(|| params.get("login_id")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            success: params
                .and_then(|params| params.get("success"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        };
    }
    CodexLoginMessage::Other
}

/// A device code is a short run of letters, digits and dashes.
fn valid_user_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

struct CodexLoginControl {
    cancel: tokio::sync::Notify,
}

#[async_trait]
impl LoginControl for CodexLoginControl {
    async fn submit_code(&self, _code: &str) -> Result<(), LoginError> {
        Err(LoginError::NotWaitingForCode {
            provider_id: "codex_cli".to_string(),
            phase: "device-code sign-in needs no pasted code".to_string(),
        })
    }

    fn cancel(&self) {
        self.cancel.notify_one();
    }
}

/// Kill the process group on drop unless the child was reaped.
struct GroupKill(Option<i32>);

impl GroupKill {
    fn kill(&self) {
        if let Some(pgid) = self.0 {
            // SAFETY: kill(2) with a negative pid signals a process group;
            // ESRCH for an exited group is ignored.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Start `codex app-server`, request a ChatGPT device code and return once it
/// has been issued. The returned completion resolves when the app-server
/// reports `account/login/completed`. Nothing the server prints is logged.
pub async fn begin_codex_login(options: CodexLoginOptions) -> Result<LoginHandle, LoginError> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    const PROVIDER_ID: &str = "codex_cli";
    let CodexLoginOptions {
        program,
        identity,
        location,
        challenge_timeout,
    } = options;

    host_cli::prepare_login_location(PROVIDER_ID, &identity, &location).await?;

    let mut command = host_cli::host_command(&program, &identity, Some(&location));
    command
        .arg("app-server")
        .current_dir(&location.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LoginError::NotInstalled {
                provider_id: PROVIDER_ID.to_string(),
            }
        } else {
            LoginError::Spawn {
                provider_id: PROVIDER_ID.to_string(),
                reason: error.to_string(),
            }
        }
    })?;
    let mut group = GroupKill(child.id().and_then(|pid| i32::try_from(pid).ok()));
    let io_error = |operation: &str, reason: String| LoginError::Io {
        provider_id: PROVIDER_ID.to_string(),
        operation: operation.to_string(),
        reason,
    };

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io_error("opening the app-server input", "stdin was not piped".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        io_error(
            "opening the app-server output",
            "stdout was not piped".into(),
        )
    })?;
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
    }

    let requests = format!(
        "{}\n{}\n{}\n",
        r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"temps","title":"Temps","version":"1"}}}"#,
        r#"{"method":"initialized","params":{}}"#,
        format_args!(
            r#"{{"id":{LOGIN_START_REQUEST_ID},"method":"account/login/start","params":{{"type":"chatgptDeviceCode"}}}}"#
        ),
    );
    stdin
        .write_all(requests.as_bytes())
        .await
        .map_err(|error| io_error("requesting a device code", error.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|error| io_error("requesting a device code", error.to_string()))?;

    let mut lines = BufReader::new(stdout.take(MAX_SCANNED_OUTPUT_BYTES as u64)).lines();
    let challenge_wait = async {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match parse_codex_login_message(&line) {
                    CodexLoginMessage::Challenge {
                        login_id,
                        verification_url,
                        user_code,
                    } => return Ok((login_id, verification_url, user_code)),
                    CodexLoginMessage::Refused { code } => {
                        return Err(LoginError::CliRejected {
                            provider_id: PROVIDER_ID.to_string(),
                            exit_code: i32::try_from(code).unwrap_or(-1),
                        })
                    }
                    CodexLoginMessage::Completed { .. } | CodexLoginMessage::Other => {}
                },
                Ok(None) | Err(_) => {
                    return Err(LoginError::NoChallenge {
                        provider_id: PROVIDER_ID.to_string(),
                        waited_secs: challenge_timeout.as_secs(),
                    })
                }
            }
        }
    };
    let issued = match tokio::time::timeout(challenge_timeout, challenge_wait).await {
        Ok(Ok(issued)) => issued,
        Ok(Err(error)) => {
            group.kill();
            let _ = child.wait().await;
            group.disarm();
            return Err(match (&error, child_exit_hint(&mut child)) {
                (LoginError::NoChallenge { .. }, Some(126 | 127)) => LoginError::NotInstalled {
                    provider_id: PROVIDER_ID.to_string(),
                },
                _ => error,
            });
        }
        Err(_) => {
            group.kill();
            let _ = child.wait().await;
            group.disarm();
            return Err(LoginError::NoChallenge {
                provider_id: PROVIDER_ID.to_string(),
                waited_secs: challenge_timeout.as_secs(),
            });
        }
    };
    let (login_id, verification_url, user_code) = issued;

    let verification_url =
        match validate_verification_url(PROVIDER_ID, &verification_url, CODEX_LOGIN_HOSTS) {
            Ok(url) => url,
            Err(error) => {
                group.kill();
                let _ = child.wait().await;
                group.disarm();
                return Err(error);
            }
        };
    if !valid_user_code(&user_code) {
        group.kill();
        let _ = child.wait().await;
        group.disarm();
        return Err(LoginError::InvalidUrl {
            provider_id: PROVIDER_ID.to_string(),
            reason: "the device code has an unexpected shape".to_string(),
        });
    }

    let control = std::sync::Arc::new(CodexLoginControl {
        cancel: tokio::sync::Notify::new(),
    });
    let waiter = control.clone();
    let completion = Box::pin(async move {
        // Keep stdin open for the life of the sign-in; closing it ends the
        // app-server before the browser step finishes.
        let _stdin = stdin;
        let outcome = async {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if let CodexLoginMessage::Completed {
                            login_id: completed_id,
                            success,
                        } = parse_codex_login_message(&line)
                        {
                            let same_login = match (&login_id, &completed_id) {
                                (Some(expected), Some(actual)) => expected == actual,
                                _ => true,
                            };
                            if same_login {
                                return if success {
                                    Ok(())
                                } else {
                                    Err(LoginError::NotCompleted {
                                        provider_id: PROVIDER_ID.to_string(),
                                    })
                                };
                            }
                        }
                    }
                    Ok(None) | Err(_) => {
                        return Err(LoginError::NotCompleted {
                            provider_id: PROVIDER_ID.to_string(),
                        })
                    }
                }
            }
        };
        let result = tokio::select! {
            result = outcome => result,
            _ = waiter.cancel.notified() => Err(LoginError::Cancelled {
                provider_id: PROVIDER_ID.to_string(),
            }),
        };
        // The app-server is only needed until the credential is written.
        group.kill();
        let _ = child.wait().await;
        group.disarm();
        result
    });

    Ok(LoginHandle {
        challenge: LoginChallenge {
            verification_url,
            user_code: Some(user_code),
            requires_code: false,
        },
        control,
        completion,
    })
}

fn child_exit_hint(child: &mut tokio::process::Child) -> Option<i32> {
    child
        .try_wait()
        .ok()
        .flatten()
        .and_then(|status| status.code())
}

#[async_trait]
impl AiCliProvider for CodexCliProvider {
    fn name(&self) -> &str {
        "codex_cli"
    }

    async fn check_installed(&self) -> bool {
        tokio::time::timeout(
            STATUS_TIMEOUT,
            codex_command()
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
        probe_codex_status(codex_command, true).await
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn begin_login(&self, config_dir: &std::path::Path) -> Result<LoginHandle, LoginError> {
        let location =
            ProviderLocation::from_config_dir("codex_cli", config_dir).ok_or_else(|| {
                LoginError::Io {
                    provider_id: "codex_cli".to_string(),
                    operation: "resolving the sign-in directory".to_string(),
                    reason: format!(
                        "{} is not a Codex home directory (expected a directory named .codex)",
                        config_dir.display()
                    ),
                }
            })?;
        begin_codex_login(CodexLoginOptions {
            program: std::ffi::OsString::from("codex"),
            identity: HostIdentity::current(),
            location,
            challenge_timeout: CHALLENGE_TIMEOUT,
        })
        .await
    }

    async fn verify_login(&self, config_dir: &std::path::Path) -> Result<AiCliStatus, LoginError> {
        let location =
            ProviderLocation::from_config_dir("codex_cli", config_dir).ok_or_else(|| {
                LoginError::Io {
                    provider_id: "codex_cli".to_string(),
                    operation: "resolving the sign-in directory".to_string(),
                    reason: format!("{} is not a Codex home directory", config_dir.display()),
                }
            })?;
        let identity = HostIdentity::current();
        Ok(probe_codex_status(
            || host_cli::host_command("codex", &identity, Some(&location)),
            false,
        )
        .await)
    }

    async fn discover_model_capabilities(&self) -> Vec<super::AiCliModelCapability> {
        discover_models()
            .await
            .into_iter()
            .map(|model| super::AiCliModelCapability {
                id: model.id,
                name: model.name,
                reasoning_options: model.reasoning_efforts,
                default_reasoning_option: model.default_reasoning_effort,
            })
            .collect()
    }

    fn extract_assistant_text(&self, line: &str) -> Option<String> {
        extract_assistant_text(line)
    }

    fn extract_native_tool_events(&self, line: &str) -> Vec<NativeToolEvent> {
        extract_native_tool_events(line)
    }

    fn dropped_tool_use_name(&self, line: &str) -> Option<String> {
        dropped_tool_use_name(line)
    }

    async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = build_run_command(&config);

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::AiCliNotInstalled {
                    provider: self.name().to_string(),
                }
            } else {
                AgentError::Io(e)
            }
        })?;

        let stdout_handle = child.stdout.take().ok_or_else(|| {
            AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Codex stdout was not piped",
            ))
        })?;
        let stderr_handle = child.stderr.take().ok_or_else(|| {
            AgentError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Codex stderr was not piped",
            ))
        })?;
        let on_event = config.on_event.clone();

        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout_handle).lines();
            let mut output = String::new();
            while let Some(line) = lines.next_line().await? {
                output.push_str(&line);
                output.push('\n');
                if let Some(ref callback) = on_event {
                    callback(line).await;
                }
            }
            Ok::<String, std::io::Error>(output)
        });
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr_handle).lines();
            let mut output = String::new();
            while let Some(line) = lines.next_line().await? {
                output.push_str(&line);
                output.push('\n');
            }
            Ok::<String, std::io::Error>(output)
        });

        let status = match tokio::time::timeout(config.timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(AgentError::Io(error)),
            Err(_) => {
                let _ = child.kill().await;
                return Err(AgentError::AiCliTimeout {
                    provider: self.name().to_string(),
                    timeout_secs: config.timeout.as_secs(),
                });
            }
        };
        let stdout = stdout_task.await.map_err(|error| AgentError::Validation {
            message: format!("Codex stdout reader task failed: {error}"),
        })??;
        let stderr = stderr_task.await.map_err(|error| AgentError::Validation {
            message: format!("Codex stderr reader task failed: {error}"),
        })??;
        let exit_code = status.code().unwrap_or(-1);

        if !status.success() {
            return Err(AgentError::AiCliFailed {
                provider: self.name().to_string(),
                exit_code,
                stderr: crate::ai_cli::summarize_cli_failure(self.name(), &stderr),
            });
        }

        // Try to parse token usage from JSON lines in stdout
        let (tokens_input, tokens_output, model) = parse_codex_output(&stdout);

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
        // Codex CLI doesn't have a --continue flag, so just run a new session
        // in the same work directory. The context is lost, but it's the best we can do.
        self.run(config).await
    }
}

fn codex_auth_method(status_output: &str) -> Option<String> {
    let normalized = status_output.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("not logged in") || normalized.contains("not authenticated") {
        return None;
    }
    if normalized.contains("chatgpt") {
        Some("chatgpt_subscription".to_string())
    } else if normalized.contains("api key") {
        Some("api_key_file".to_string())
    } else if normalized.contains("logged in") || normalized.contains("authenticated") {
        Some("host_login".to_string())
    } else {
        None
    }
}

fn codex_doctor_auth_method(report: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(report).ok()?;
    let auth = value.get("checks")?.get("auth.credentials")?;
    if auth.get("status")?.as_str()? != "ok" {
        return None;
    }
    let details = auth.get("details")?;
    let mode = details
        .get("stored auth mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match mode.as_str() {
        "chatgpt" => Some("chatgpt_subscription".to_string()),
        "api_key" | "apikey" | "api key" => Some("api_key_file".to_string()),
        _ if details
            .get("stored ChatGPT tokens")
            .and_then(serde_json::Value::as_str)
            == Some("true") =>
        {
            Some("chatgpt_subscription".to_string())
        }
        _ if details
            .get("stored API key")
            .and_then(serde_json::Value::as_str)
            == Some("true") =>
        {
            Some("api_key_file".to_string())
        }
        _ => Some("host_login".to_string()),
    }
}

/// Parse Codex CLI JSON output for token usage and model information.
pub fn parse_codex_output(output: &str) -> (Option<i32>, Option<i32>, Option<String>) {
    let mut tokens_input: Option<i32> = None;
    let mut tokens_output: Option<i32> = None;
    let mut model: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(usage) = value.get("usage") {
                if tokens_input.is_none() {
                    tokens_input = usage
                        .get("prompt_tokens")
                        .or_else(|| usage.get("input_tokens"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32);
                }
                if tokens_output.is_none() {
                    tokens_output = usage
                        .get("completion_tokens")
                        .or_else(|| usage.get("output_tokens"))
                        .and_then(|v| v.as_i64())
                        .map(|v| v as i32);
                }
            }
            if model.is_none() {
                model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
        }
    }

    (tokens_input, tokens_output, model)
}

/// Extract assistant text from one line of Codex CLI's `--json` output.
/// Only `type":"item.completed"` events whose `item.type` is
/// `"agent_message"` carry user-facing text — everything else
/// (`thread.started`, `turn.completed`, tool-call items, …) returns `None`.
pub fn extract_assistant_text(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("item.completed") {
        return None;
    }
    let item = value.get("item")?;
    if item.get("type").and_then(|v| v.as_str()) != Some("agent_message") {
        return None;
    }
    let text = item.get("text").and_then(|v| v.as_str())?;
    if text.is_empty() {
        None
    } else {
        // Each completed item is a distinct prose block, not a token delta.
        // Preserve that boundary when the shared chat stream concatenates it.
        Some(format!("{}\n\n", text.trim_end()))
    }
}

/// Make Codex's user-visible execution activity visible in Temps chat.
///
/// `codex exec --json` emits typed items as work starts, updates and completes.
/// Completed items
/// deliberately include the call again: consumers deduplicate calls by id,
/// while this also makes a completed-only transcript self-contained after a
/// reconnect or a provider-version change that omits the started event.
pub fn extract_native_tool_events(line: &str) -> Vec<NativeToolEvent> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    let event_type = value.get("type").and_then(serde_json::Value::as_str);
    if !matches!(
        event_type,
        Some("item.started" | "item.updated" | "item.completed")
    ) {
        return Vec::new();
    }
    let Some(item) = value.get("item") else {
        return Vec::new();
    };
    let Some(id) = item.get("id").and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    let completed = event_type == Some("item.completed");
    let (name, arguments, result) = match item.get("type").and_then(serde_json::Value::as_str) {
        Some("command_execution") => {
            let Some(command) = item.get("command").and_then(serde_json::Value::as_str) else { return Vec::new(); };
            let output = item.get("aggregated_output").or_else(|| item.get("output"))
                .and_then(serde_json::Value::as_str).unwrap_or_default();
            let exit = item.get("exit_code").and_then(serde_json::Value::as_i64)
                .map(|code| format!("\n\nProcess exited with code {code}."))
                .unwrap_or_default();
            ("Bash".to_string(), serde_json::json!({"command":command}),
                completed.then(|| format!("{}{exit}", output.trim_end())))
        }
        Some("mcp_tool_call") => {
            let (Some(server), Some(tool)) = (item.get("server").and_then(serde_json::Value::as_str), item.get("tool").and_then(serde_json::Value::as_str)) else { return Vec::new(); };
            let result = completed.then(|| {
                // Binary image/audio blocks can be enormous. Preserve text and
                // structured results while describing other content by type.
                let content: Vec<serde_json::Value> = item.pointer("/result/content")
                    .and_then(serde_json::Value::as_array).into_iter().flatten()
                    .map(|block| match block.get("type").and_then(serde_json::Value::as_str) {
                        Some("text") => serde_json::json!({"type":"text", "text":block.get("text")}),
                        kind => serde_json::json!({"type":kind, "note":"Non-text tool output"}),
                    }).collect();
                serde_json::json!({"status":item.get("status"),"content":content,
                    "structured_content":item.pointer("/result/structured_content"),"error":item.get("error")}).to_string()
            });
            (format!("mcp__{server}__{tool}"), item.get("arguments").cloned().unwrap_or_else(|| serde_json::json!({})), result)
        }
        Some("file_change") => {
            let Some(changes) = item.get("changes").filter(|value| value.is_array()) else { return Vec::new(); };
            ("File changes".to_string(), serde_json::json!({"changes":changes}),
                completed.then(|| serde_json::json!({"status":item.get("status"),"changes":changes}).to_string()))
        }
        Some("web_search") => ("Web search".to_string(), serde_json::json!({"query":item.get("query"),"action":item.get("action")}),
            completed.then(|| serde_json::json!({"status":"completed","query":item.get("query"),"action":item.get("action")}).to_string())),
        Some("todo_list") => {
            let Some(items) = item.get("items").filter(|value| value.is_array()) else { return Vec::new(); };
            ("Plan".to_string(), serde_json::json!({"items":items}),
                Some(serde_json::json!({"status":if completed {"completed"} else {"in_progress"},"items":items}).to_string()))
        }
        Some("collab_tool_call") => {
            let Some(tool) = item.get("tool").and_then(serde_json::Value::as_str) else { return Vec::new(); };
            (format!("Agent · {tool}"), serde_json::json!({"receiver_thread_ids":item.get("receiver_thread_ids"),"prompt":item.get("prompt")}),
                completed.then(|| serde_json::json!({"status":item.get("status"),"agents_states":item.get("agents_states")}).to_string()))
        }
        Some("error") => ("Codex error".to_string(), serde_json::json!({}),
            Some(serde_json::json!({"message":item.get("message")}).to_string())),
        // Private reasoning is not execution activity. Unknown future variants
        // remain observable through the dropped-item diagnostic below.
        _ => return Vec::new(),
    };
    let mut events = vec![NativeToolEvent::Call {
        id: id.to_string(),
        name,
        arguments: arguments.to_string(),
    }];
    if let Some(result) = result {
        events.push(NativeToolEvent::Result {
            call_id: id.to_string(),
            result,
        });
    }
    events
}

/// Name the item type of a completed non-text item, when `line` is a
/// `type":"item.completed"` event whose `item.type` is something other than
/// `agent_message`, `reasoning`, or a supported native activity. This catches
/// malformed events and new upstream variants without logging their payload.
///
/// Used only for diagnostic logging (ADR-038) — never logs `item` fields
/// beyond the type name, which may carry command output or file contents.
pub fn dropped_tool_use_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some("item.completed") {
        return None;
    }
    let item_type = value.get("item")?.get("type")?.as_str()?;
    if item_type == "agent_message"
        || item_type == "reasoning"
        || !extract_native_tool_events(line).is_empty()
    {
        None
    } else {
        Some(item_type.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_output_with_usage() {
        let output = r#"{"model":"gpt-4o","usage":{"prompt_tokens":200,"completion_tokens":80}}"#;
        let (input, output_tokens, model) = parse_codex_output(output);
        assert_eq!(input, Some(200));
        assert_eq!(output_tokens, Some(80));
        assert_eq!(model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn test_parse_codex_output_empty() {
        let output = "plain text output";
        let (input, output_tokens, model) = parse_codex_output(output);
        assert!(input.is_none());
        assert!(output_tokens.is_none());
        assert!(model.is_none());
    }

    #[test]
    fn test_codex_provider_name() {
        let provider = CodexCliProvider;
        assert_eq!(provider.name(), "codex_cli");
    }

    #[test]
    fn test_run_command_allows_isolated_non_git_scratch_directory() {
        let scratch = tempfile::tempdir().expect("create scratch directory");
        let config = AiRunConfig {
            work_dir: scratch.path().to_owned(),
            prompt: "hello".to_string(),
            api_key: String::new(),
            max_turns: 1,
            timeout: std::time::Duration::from_secs(30),
            model: Some("gpt-5.6-sol".to_string()),
            thinking_level: Some("low".to_string()),
            permission_mode: Some("auto".to_string()),
            on_event: None,
            permission_bridge: None,
            resume_session_id: None,
            mcp_server: Some(crate::ai_cli::McpServerConfig {
                url: "http://127.0.0.1:32123/mcp/turn".to_string(),
                authorization_token: "ephemeral-test-token".to_string(),
            }),
        };

        let command = build_run_command(&config);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(
            args.iter().any(|arg| arg == "--skip-git-repo-check"),
            "Codex must accept Temps' isolated non-Git scratch directory: {args:?}"
        );
        assert!(args.iter().any(|arg| arg == "--ignore-user-config"));
        assert!(args.iter().any(|arg| arg == "--ignore-rules"));
        assert!(args.iter().any(|arg| arg == "--ephemeral"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--model", "gpt-5.6-sol"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace-write"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--config", "approval_policy=\"on-request\""]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--config", "approvals_reviewer=\"auto_review\""]));
        assert!(!args.iter().any(|arg| arg == "--full-auto"));
        assert!(args.iter().any(|arg| {
            arg == "mcp_servers.temps_chat.url=\"http://127.0.0.1:32123/mcp/turn\""
        }));
        assert!(args.iter().any(|arg| {
            arg == "mcp_servers.temps_chat.bearer_token_env_var=\"TEMPS_CHAT_MCP_TOKEN\""
        }));
        assert!(command.as_std().get_envs().any(|(name, value)| {
            name == "TEMPS_CHAT_MCP_TOKEN"
                && value.is_some_and(|value| value == "ephemeral-test-token")
        }));
    }

    #[test]
    fn test_codex_auth_method_detects_host_login_source() {
        assert_eq!(
            codex_auth_method("Logged in using ChatGPT\n").as_deref(),
            Some("chatgpt_subscription")
        );
        assert_eq!(
            codex_auth_method("Logged in using an API key\n").as_deref(),
            Some("api_key_file")
        );
        assert_eq!(codex_auth_method("  "), None);
        assert_eq!(codex_auth_method("warning: PATH alias unavailable"), None);
        assert_eq!(codex_auth_method("Not logged in"), None);
    }

    #[test]
    fn test_codex_doctor_auth_method_detects_redacted_chatgpt_login() {
        let report = r#"{
          "schemaVersion": 1,
          "overallStatus": "fail",
          "checks": {
            "auth.credentials": {
              "status": "ok",
              "details": {
                "auth file": "/host/.codex/auth.json",
                "stored API key": "false",
                "stored ChatGPT tokens": "true",
                "stored auth mode": "chatgpt"
              }
            }
          }
        }"#;

        assert_eq!(
            codex_doctor_auth_method(report).as_deref(),
            Some("chatgpt_subscription")
        );
    }

    #[test]
    fn test_codex_doctor_auth_method_rejects_unconfigured_or_invalid_reports() {
        let unconfigured = r#"{
          "checks": {
            "auth.credentials": {
              "status": "fail",
              "details": { "stored auth mode": "chatgpt" }
            }
          }
        }"#;
        assert_eq!(codex_doctor_auth_method(unconfigured), None);
        assert_eq!(codex_doctor_auth_method("not json"), None);
        assert_eq!(codex_doctor_auth_method(r#"{"checks":{}}"#), None);
    }

    #[test]
    fn test_codex_doctor_auth_method_detects_api_key_without_exposing_path() {
        let report = r#"{
          "checks": {
            "auth.credentials": {
              "status": "ok",
              "details": {
                "auth file": "/secret/location/auth.json",
                "stored API key": "true",
                "stored ChatGPT tokens": "false",
                "stored auth mode": "api_key"
              }
            }
          }
        }"#;

        assert_eq!(
            codex_doctor_auth_method(report).as_deref(),
            Some("api_key_file")
        );
    }

    #[test]
    fn test_parse_codex_output_multi_line_stream() {
        // Real Codex --json output is one event per line. The parser should
        // find usage on any line and model on the first line that has it.
        let output = "\
{\"type\":\"thread.started\",\"thread_id\":\"abc\"}\n\
{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}\n\
{\"type\":\"turn.completed\",\"model\":\"gpt-5-codex\",\"usage\":{\"prompt_tokens\":1234,\"completion_tokens\":567}}\n";
        let (input, output_tokens, model) = parse_codex_output(output);
        assert_eq!(input, Some(1234));
        assert_eq!(output_tokens, Some(567));
        assert_eq!(model.as_deref(), Some("gpt-5-codex"));
    }

    #[test]
    fn test_parse_codex_output_prefers_first_usage() {
        // Parser keeps the first non-None value it sees; later usage events
        // (e.g. per-turn counts in a multi-turn session) should not clobber
        // the first one. This documents current behavior so a future refactor
        // to "accumulate" doesn't silently change the contract.
        let output = "\
{\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}\n\
{\"usage\":{\"prompt_tokens\":999,\"completion_tokens\":888}}\n";
        let (input, output_tokens, _model) = parse_codex_output(output);
        assert_eq!(input, Some(100));
        assert_eq!(output_tokens, Some(50));
    }

    #[test]
    fn test_parse_codex_output_accepts_input_output_token_aliases() {
        // Some Codex builds emit input_tokens/output_tokens instead of the
        // prompt_tokens/completion_tokens aliases. The parser accepts both.
        let output = r#"{"model":"gpt-5","usage":{"input_tokens":77,"output_tokens":88}}"#;
        let (input, output_tokens, model) = parse_codex_output(output);
        assert_eq!(input, Some(77));
        assert_eq!(output_tokens, Some(88));
        assert_eq!(model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn test_parse_codex_output_skips_non_json_lines() {
        // Stderr/debug lines intermixed with JSON must not crash or confuse
        // the parser.
        let output = "\
Warning: unable to contact upstream\n\
{\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":21},\"model\":\"gpt-5\"}\n\
not json either\n";
        let (input, output_tokens, model) = parse_codex_output(output);
        assert_eq!(input, Some(42));
        assert_eq!(output_tokens, Some(21));
        assert_eq!(model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn test_parse_codex_output_missing_usage_still_captures_model() {
        // Model can appear on events without usage; we still want to learn it.
        let output = "{\"type\":\"thread.started\",\"model\":\"gpt-5-codex\"}\n";
        let (input, output_tokens, model) = parse_codex_output(output);
        assert!(input.is_none());
        assert!(output_tokens.is_none());
        assert_eq!(model.as_deref(), Some("gpt-5-codex"));
    }

    #[test]
    fn test_parse_codex_output_malformed_json_is_ignored() {
        let output =
            "{not valid json}\n{\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n";
        let (input, output_tokens, _model) = parse_codex_output(output);
        assert_eq!(input, Some(10));
        assert_eq!(output_tokens, Some(5));
    }

    #[test]
    fn test_extract_assistant_text_from_agent_message() {
        let line = r#"{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}"#;
        assert_eq!(extract_assistant_text(line).as_deref(), Some("hi\n\n"));
    }

    #[test]
    fn test_extract_assistant_text_ignores_non_agent_message_items() {
        let thread_started = r#"{"type":"thread.started","thread_id":"abc"}"#;
        let turn_completed = r#"{"type":"turn.completed","model":"gpt-5-codex"}"#;
        let tool_call =
            r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls"}}"#;
        assert_eq!(extract_assistant_text(thread_started), None);
        assert_eq!(extract_assistant_text(turn_completed), None);
        assert_eq!(extract_assistant_text(tool_call), None);
    }

    #[test]
    fn test_extract_native_tool_events_tracks_codex_command_lifecycle() {
        let started = r#"{"type":"item.started","item":{"id":"item_7","type":"command_execution","command":"npm run build","status":"in_progress"}}"#;
        assert_eq!(
            extract_native_tool_events(started),
            vec![NativeToolEvent::Call {
                id: "item_7".to_string(),
                name: "Bash".to_string(),
                arguments: r#"{"command":"npm run build"}"#.to_string(),
            }]
        );

        let completed = r#"{"type":"item.completed","item":{"id":"item_7","type":"command_execution","command":"npm run build","aggregated_output":"compiled\n","exit_code":0,"status":"completed"}}"#;
        assert_eq!(
            extract_native_tool_events(completed),
            vec![
                NativeToolEvent::Call {
                    id: "item_7".to_string(),
                    name: "Bash".to_string(),
                    arguments: r#"{"command":"npm run build"}"#.to_string(),
                },
                NativeToolEvent::Result {
                    call_id: "item_7".to_string(),
                    result: "compiled\n\nProcess exited with code 0.".to_string(),
                },
            ]
        );
    }

    #[test]
    fn codex_activity_preserves_mcp_results_and_errors_without_binary_blobs() {
        let line = serde_json::json!({"type":"item.completed","item":{
            "id":"mcp-1","type":"mcp_tool_call","server":"temps","tool":"get_projects",
            "arguments":{"page":1},"status":"failed","error":{"message":"Access denied"},
            "result":{"content":[{"type":"text","text":"No projects"},{"type":"image","data":"BASE64_SHOULD_NOT_BE_STORED"}],"structured_content":{"total":0}}
        }}).to_string();
        let events = extract_native_tool_events(&line);
        assert!(
            matches!(&events[0], NativeToolEvent::Call{name,arguments,..} if name == "mcp__temps__get_projects" && arguments == r#"{"page":1}"#)
        );
        let NativeToolEvent::Result { result, .. } = &events[1] else {
            panic!("missing MCP result")
        };
        assert!(result.contains("No projects"));
        assert!(result.contains("Access denied"));
        assert!(result.contains("failed"));
        assert!(!result.contains("BASE64_SHOULD_NOT_BE_STORED"));
        assert_eq!(dropped_tool_use_name(&line), None);
    }

    #[test]
    fn codex_activity_preserves_file_search_agent_and_plan_items() {
        for (item, expected_name) in [
            (
                serde_json::json!({"id":"file-1","type":"file_change","changes":[{"path":"src/app.tsx","kind":"update"}],"status":"completed"}),
                "File changes",
            ),
            (
                serde_json::json!({"id":"search-1","type":"web_search","query":"Next.js documentation","action":{"type":"search","query":"Next.js documentation"}}),
                "Web search",
            ),
            (
                serde_json::json!({"id":"agent-1","type":"collab_tool_call","tool":"spawn_agent","receiver_thread_ids":["child-1"],"prompt":"Check tests","status":"completed","agents_states":{"child-1":{"status":"running"}}}),
                "Agent · spawn_agent",
            ),
            (
                serde_json::json!({"id":"plan-1","type":"todo_list","items":[{"text":"Build page","completed":true}]}),
                "Plan",
            ),
        ] {
            let line = serde_json::json!({"type":"item.completed","item":item}).to_string();
            let events = extract_native_tool_events(&line);
            assert!(
                matches!(&events[0], NativeToolEvent::Call { name,.. } if name == expected_name)
            );
            assert!(matches!(&events[1], NativeToolEvent::Result { .. }));
            assert_eq!(dropped_tool_use_name(&line), None);
        }
    }

    #[test]
    fn codex_plan_updates_preserve_current_completion_state() {
        let line = r#"{"type":"item.updated","item":{"id":"plan","type":"todo_list","items":[{"text":"Build page","completed":false}]}}"#;
        let events = extract_native_tool_events(line);
        let NativeToolEvent::Result { call_id, result } = &events[1] else {
            panic!("missing plan update")
        };
        assert_eq!(call_id, "plan");
        let value: serde_json::Value = serde_json::from_str(result).unwrap();
        assert_eq!(value["status"], "in_progress");
        assert_eq!(value["items"][0]["completed"], false);
    }

    #[test]
    fn test_extract_native_tool_events_ignores_unrelated_and_malformed_items() {
        assert!(extract_native_tool_events("not json").is_empty());
        assert!(extract_native_tool_events(
            r#"{"type":"item.started","item":{"id":"item_8","type":"reasoning","text":"thinking"}}"#
        )
        .is_empty());
        assert!(extract_native_tool_events(
            r#"{"type":"item.started","item":{"type":"command_execution","command":"pwd"}}"#
        )
        .is_empty());
    }

    #[test]
    fn test_dropped_tool_use_name_from_command_execution() {
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","command":"rm -rf /secret"}}"#;
        let name = dropped_tool_use_name(line);
        assert_eq!(name.as_deref(), Some("command_execution"));
        assert!(!name.unwrap().contains("rm -rf"));
    }

    #[test]
    fn test_dropped_tool_use_name_none_for_agent_message_and_reasoning() {
        let agent_message =
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}"#;
        let reasoning = r#"{"type":"item.completed","item":{"type":"reasoning","text":"..."}}"#;
        assert_eq!(dropped_tool_use_name(agent_message), None);
        assert_eq!(dropped_tool_use_name(reasoning), None);
    }

    #[test]
    fn test_dropped_tool_use_name_none_for_other_events() {
        let thread_started = r#"{"type":"thread.started","thread_id":"abc"}"#;
        assert_eq!(dropped_tool_use_name(thread_started), None);
    }

    fn runtime_args(permission: &str, effort: &str) -> Vec<String> {
        let mut command = Command::new("codex");
        apply_runtime_args(&mut command, Some(permission), Some(effort));
        command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn codex_default_runtime_auto_reviews_in_workspace_sandbox() {
        assert_eq!(
            runtime_args("auto", "high"),
            [
                "--sandbox",
                "workspace-write",
                "--config",
                "approval_policy=\"on-request\"",
                "--config",
                "approvals_reviewer=\"auto_review\"",
                "--config",
                "model_reasoning_effort=\"high\""
            ]
        );
    }

    #[test]
    fn codex_auto_review_uses_supported_config_keys() {
        let args = runtime_args("auto-review", "medium");
        assert_eq!(
            args,
            [
                "--sandbox",
                "workspace-write",
                "--config",
                "approval_policy=\"on-request\"",
                "--config",
                "approvals_reviewer=\"auto_review\"",
                "--config",
                "model_reasoning_effort=\"medium\"",
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--ask-for-approval"));
    }

    #[test]
    fn codex_full_access_uses_explicit_bypass_flag() {
        assert_eq!(
            runtime_args("full-access", "xhigh"),
            [
                "--dangerously-bypass-approvals-and-sandbox",
                "--config",
                "model_reasoning_effort=\"xhigh\"",
            ]
        );
    }

    #[test]
    fn parses_app_server_model_list_with_model_specific_efforts() {
        let response = serde_json::json!({
            "id": 2,
            "result": {
                "data": [{
                    "id": "gpt-5.6-sol",
                    "displayName": "GPT-5.6 Sol",
                    "supportedReasoningEfforts": [
                        {"reasoningEffort": "low"},
                        {"reasoningEffort": "high"}
                    ],
                    "defaultReasoningEffort": "high"
                }]
            }
        });
        assert_eq!(
            parse_model_list_response(&response),
            vec![CodexModelInfo {
                id: "gpt-5.6-sol".to_string(),
                name: "GPT-5.6 Sol".to_string(),
                reasoning_efforts: vec!["low".to_string(), "high".to_string()],
                default_reasoning_effort: Some("high".to_string()),
            }]
        );
    }

    #[test]
    fn parses_workspace_model_capabilities_from_app_server_output() {
        let output = concat!(
            "{\"id\":1,\"result\":{}}\n",
            "{\"id\":2,\"result\":{\"data\":[{\"id\":\"gpt-5.6-sol\",",
            "\"displayName\":\"GPT-5.6 Sol\",\"supportedReasoningEfforts\":[",
            "{\"reasoningEffort\":\"low\"},{\"reasoningEffort\":\"high\"}],",
            "\"defaultReasoningEffort\":\"high\"}]}}\n"
        );

        assert_eq!(
            parse_model_capabilities_from_app_server_output(output),
            vec![super::super::AiCliModelCapability {
                id: "gpt-5.6-sol".to_string(),
                name: "GPT-5.6 Sol".to_string(),
                reasoning_options: vec!["low".to_string(), "high".to_string()],
                default_reasoning_option: Some("high".to_string()),
            }]
        );
    }
}
