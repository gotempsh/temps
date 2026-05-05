use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use temps_agents::ai_cli::{
    catalog::{find_provider, CredentialFormat},
    OnEventCallback,
};
use temps_agents::sandbox::{
    SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider, SANDBOX_HOME,
    SANDBOX_WORK_DIR,
};
use temps_core::EncryptionService;

use crate::error::WorkspaceError;

/// POSIX-style single-quoted shell escape. Wraps the input in single quotes
/// and re-encodes any embedded single quote as `'\''`. Used to splice
/// user-controlled tokens (e.g. `ai_model`) into a `bash -lc` string without
/// allowing shell metacharacter injection.
fn posix_shell_escape(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Canonical Temps CLI skill, embedded at compile time from
/// `temps/skills/temps-cli/SKILL.md`. Kept as a standalone constant (in
/// addition to its entry in [`BUNDLED_SKILLS`]) so the existing content
/// assertions and external callers that specifically want the CLI reference
/// don't have to go through a lookup.
pub const TEMPS_PLATFORM_SKILL: &str = include_str!("../../../../skills/temps-cli/SKILL.md");

/// All skills shipped under `temps/skills/` that we auto-inject into workspace
/// sandboxes. The directory name is the skill's frontmatter `name:` field —
/// Claude Code's skill discovery requires the containing directory to match
/// that name exactly, so the tuple key is reused as the on-disk directory.
///
/// Auto-injected into workspace sandboxes so every session sees the same
/// skill library the rest of the platform ships (CLI reference, deploy
/// recipes, SDK integration guides, plugin authoring, etc.).
///
/// We deliberately do NOT pre-install `@temps-sdk/cli` in the sandbox image —
/// the CLI skill instructs Claude to call it via `bunx @temps-sdk/cli@latest`
/// (or `npx @temps-sdk/cli@latest`), so each session always picks up the
/// latest published version without rebuilding container images.
pub const BUNDLED_SKILLS: &[(&str, &str)] = &[
    (
        "temps-cli",
        include_str!("../../../../skills/temps-cli/SKILL.md"),
    ),
    (
        "temps-platform-setup",
        include_str!("../../../../skills/temps-platform-setup/SKILL.md"),
    ),
    (
        "temps-mcp-setup",
        include_str!("../../../../skills/temps-mcp-setup/SKILL.md"),
    ),
    (
        "temps-plugin",
        include_str!("../../../../skills/temps-plugin/SKILL.md"),
    ),
    (
        "deploy-to-temps",
        include_str!("../../../../skills/deploy-to-temps/SKILL.md"),
    ),
    (
        "add-custom-domain",
        include_str!("../../../../skills/add-custom-domain/SKILL.md"),
    ),
    (
        "add-node-sdk",
        include_str!("../../../../skills/add-node-sdk/SKILL.md"),
    ),
    (
        "add-react-analytics",
        include_str!("../../../../skills/add-react-analytics/SKILL.md"),
    ),
    (
        "add-session-recording",
        include_str!("../../../../skills/add-session-recording/SKILL.md"),
    ),
    (
        "add-error-tracking",
        include_str!("../../../../skills/add-error-tracking/SKILL.md"),
    ),
];

/// Lightweight project descriptor used when injecting the global CLAUDE.md
/// so the agent knows exactly which Temps project the sandbox belongs to.
#[derive(Debug, Clone, Copy)]
pub struct ProjectContext<'a> {
    pub id: i32,
    pub slug: &'a str,
    pub name: &'a str,
    pub repo_owner: &'a str,
    pub repo_name: &'a str,
    pub branch: &'a str,
}

/// Tracks a live workspace session's sandbox state.
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub session_id: i32,
    pub project_id: i32,
    pub handle: SandboxHandle,
    /// Container-side work dir (e.g. `/workspace`). Same as `handle.work_dir`.
    pub work_dir: PathBuf,
    /// Host-side work dir bind-mounted into the container at `work_dir`.
    /// Kept here (rather than on SandboxHandle) to avoid touching every
    /// provider constructor. Used by the paste handler to write files
    /// directly via the bind mount. `None` for sessions recovered after a
    /// server restart — those paths can still be rehydrated lazily from the
    /// Docker container's Mounts if ever needed.
    pub host_work_dir: Option<PathBuf>,
    pub is_first_message: bool,
}

/// Compute the OAuth `expiresAt` value (ms since epoch) that we stamp into
/// `~/.claude/.credentials.json` when seeding a subscription credential.
///
/// The real expiry lives inside the access token itself (Claude CLI refreshes
/// against Anthropic's servers when the token is close to expiring); the
/// envelope's `expiresAt` only controls whether the CLI *trusts* the file on
/// load. If the stamped timestamp is in the past the CLI treats the whole
/// credentials file as stale and silently falls back to API-key / "Not
/// logged in" mode. We push it far enough out (1 year from now) that this
/// doesn't happen between sandbox creation and the next server restart.
fn oauth_expires_at_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // ~1 year in milliseconds. Safe to bump higher if needed — Claude CLI
    // doesn't care about the exact value beyond "not in the past".
    now_ms + 365 * 24 * 60 * 60 * 1000
}

/// Build the body of the per-sandbox `~/.env` file.
///
/// Emits:
/// 1. `export KEY='value'` for every entry in `env`, keys sorted for
///    determinism so refreshes produce stable diffs.
/// 2. A final `export PATH=<memory-bin-dir>:$PATH` so `memory write "..."`
///    resolves as a bare command in any shell that sources `~/.env`.
///
/// Extracted into a free function so it can be tested without constructing
/// a full [`WorkspaceSessionManager`] and sandbox provider. The output is
/// the ground truth — changes to ordering or escaping are visible in a
/// unit test, not just at runtime.
fn build_env_file_body(env: &HashMap<String, String>) -> String {
    let mut body = String::new();
    body.push_str("# Managed by Temps. Refreshed automatically — do not edit by hand.\n");
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for key in keys {
        let value = env.get(key).map(|v| v.as_str()).unwrap_or("");
        let escaped = value.replace('\'', "'\\''");
        body.push_str(&format!("export {}='{}'\n", key, escaped));
    }
    // Prepend (not append) the memory bin dir so a project that vendors
    // its own `memory` binary doesn't silently shadow ours — that would
    // break workflow memory writes and be painful to debug.
    body.push_str(&format!(
        "export PATH='{}:'\"$PATH\"\n",
        temps_core::MEMORY_SCRIPT_DIR,
    ));
    body
}

/// Manages sandbox containers for workspace sessions.
///
/// Wraps the existing SandboxProvider with workspace-specific concerns:
/// - Long-lived containers (persist across chat turns)
/// - Session tracking (maps session_id → SandboxHandle)
/// - Credential injection (Temps API token, AI provider keys)
/// - Idle timeout management
pub struct WorkspaceSessionManager {
    provider: Arc<dyn SandboxProvider>,
    #[allow(dead_code)]
    encryption_service: Arc<EncryptionService>,
    sessions: RwLock<HashMap<i32, LiveSession>>,
    idle_timeout: Duration,
}

impl WorkspaceSessionManager {
    pub fn new(
        provider: Arc<dyn SandboxProvider>,
        encryption_service: Arc<EncryptionService>,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            provider,
            encryption_service,
            sessions: RwLock::new(HashMap::new()),
            idle_timeout,
        }
    }

    /// Create a sandbox for a workspace session.
    ///
    /// The sandbox is a long-lived Docker container with:
    /// - The project repo cloned at /workspace
    /// - Temps CLI + AI provider credentials injected
    /// - Skill file auto-generated
    #[allow(clippy::too_many_arguments)]
    pub async fn create_sandbox(
        &self,
        session_id: i32,
        public_id: &str,
        project_id: i32,
        host_work_dir: PathBuf,
        env_vars: HashMap<String, String>,
        cpu_limit: Option<f32>,
        memory_limit_mb: Option<i32>,
        pids_limit: Option<i32>,
    ) -> Result<SandboxHandle, WorkspaceError> {
        let host_work_dir_for_session = host_work_dir.clone();
        // Name the container after the hex label of the public id so the
        // preview gateway's `temps-sandbox-{sid}` DNS lookup resolves when
        // users hit `ws-<hex>-<port>.<domain>`. Container name is internal
        // (Docker network only), so using the hex label here has no
        // external surface — the security win is on the URL side.
        let container_label = crate::services::public_id::hex_label(public_id).to_string();
        let config = SandboxCreateConfig {
            run_id: session_id, // Reuse run_id field for session_id
            container_name_override: Some(container_label),
            host_work_dir,
            workspace_volume: None,
            image: None,
            cpu_limit: cpu_limit.map(|v| v as f64),
            memory_limit_mb: memory_limit_mb.map(|v| v as u64),
            pids_limit: pids_limit.map(|v| v as i64),
            network_mode: None,
            env_vars,
            idle_timeout: self.idle_timeout,
        };

        let handle = self.provider.create(config).await.map_err(|e| {
            WorkspaceError::SandboxCreationFailed {
                session_id,
                reason: e.to_string(),
            }
        })?;

        let work_dir = handle.work_dir.clone();

        let live = LiveSession {
            session_id,
            project_id,
            handle: handle.clone(),
            work_dir,
            host_work_dir: Some(host_work_dir_for_session),
            is_first_message: true,
        };

        self.sessions.write().await.insert(session_id, live);

        Ok(handle)
    }

    /// Execute a command inside the session's sandbox container.
    pub async fn exec(
        &self,
        session_id: i32,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;

        self.provider
            .exec(&live.handle, cmd, env, on_output)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: e.to_string(),
            })
    }

    /// Execute a command as the sandbox container's root user.
    ///
    /// Used by setup-time tasks (credential daemon env file write) that
    /// must touch files owned by uids other than the regular sandbox
    /// user. See [`SandboxProvider::exec_as_root`] for the underlying
    /// semantics; on local-fork providers this is a no-op alias for
    /// `exec`.
    pub async fn exec_as_root(
        &self,
        session_id: i32,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;

        self.provider
            .exec_as_root(&live.handle, cmd, env, on_output)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: e.to_string(),
            })
    }

    /// Execute a command inside the session's sandbox as a specific
    /// user. `user` is `uid[:gid]` or `name[:group]` per Docker
    /// conventions. Used to write the credential daemon's env file as
    /// `temps-git` (uid 1001) directly — the only uid that can open
    /// the 0600 file in the 0700-only-temps-git directory.
    pub async fn exec_as_user(
        &self,
        session_id: i32,
        user: &str,
        cmd: Vec<String>,
        env: HashMap<String, String>,
        on_output: Option<OnEventCallback>,
    ) -> Result<SandboxExecResult, WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;

        self.provider
            .exec_as_user(&live.handle, user, cmd, env, on_output)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: e.to_string(),
            })
    }

    /// Write a file directly into the session's sandbox via the provider's
    /// native file-write API (tar streaming for Docker, fs::write for local).
    /// This avoids the bollard exec phantom-stream hang on silent processes
    /// (e.g. `bash -c "cat > ... << EOF"` heredoc writes).
    pub async fn write_file(
        &self,
        session_id: i32,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> Result<(), WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;

        self.provider
            .write_file(&live.handle, path, contents, mode)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to write {}: {}", path, e),
            })?;

        // Verify the write actually landed. If something silently swallowed
        // it (or extracted into the wrong place), surface a loud error so the
        // chat shows it instead of running with a half-initialized sandbox.
        let verify = vec!["test".to_string(), "-s".to_string(), path.to_string()];
        let result = self
            .provider
            .exec(&live.handle, verify, HashMap::new(), None)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to verify {} after write: {}", path, e),
            })?;

        if result.exit_code != 0 {
            return Err(WorkspaceError::AiCliFailed {
                session_id,
                reason: format!(
                    "Sandbox setup failed: file '{}' is missing or empty after write (exit {}). \
                     The agent will not have access to required configuration.",
                    path, result.exit_code
                ),
            });
        }

        Ok(())
    }

    /// Read a file from the session's sandbox via the provider's native
    /// download API. Returns the raw bytes.
    pub async fn read_file(&self, session_id: i32, path: &str) -> Result<Vec<u8>, WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;
        self.provider
            .read_file(&live.handle, path)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to read {}: {}", path, e),
            })
    }

    /// Upload an entire local directory tree into the session's sandbox.
    /// Used by the shared sandbox injector to install skill archives.
    pub async fn write_directory(
        &self,
        session_id: i32,
        local_dir: &std::path::Path,
        target_path: &str,
    ) -> Result<(), WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;
        self.provider
            .write_directory(&live.handle, local_dir, target_path)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to write dir {}: {}", target_path, e),
            })
    }

    /// Kill processes inside the session's sandbox matching a pattern.
    /// Best-effort: never returns a hard error, just logs.
    pub async fn kill_session_processes(
        &self,
        session_id: i32,
        pattern: &str,
        signal: temps_agents::sandbox::KillSignal,
    ) {
        let sessions = self.sessions.read().await;
        if let Some(live) = sessions.get(&session_id) {
            let _ = self
                .provider
                .kill_processes(&live.handle, pattern, signal)
                .await;
        }
    }

    /// Delete a file inside the session's sandbox. Best-effort.
    pub async fn delete_file(&self, session_id: i32, path: &str) {
        let sessions = self.sessions.read().await;
        if let Some(live) = sessions.get(&session_id) {
            let _ = self
                .provider
                .exec(
                    &live.handle,
                    vec!["rm".to_string(), "-f".to_string(), path.to_string()],
                    HashMap::new(),
                    None,
                )
                .await;
        }
    }

    /// Check whether a file exists inside the session's sandbox.
    pub async fn file_exists(&self, session_id: i32, path: &str) -> bool {
        let sessions = self.sessions.read().await;
        let Some(live) = sessions.get(&session_id) else {
            return false;
        };
        match self
            .provider
            .exec(
                &live.handle,
                vec!["test".to_string(), "-f".to_string(), path.to_string()],
                HashMap::new(),
                None,
            )
            .await
        {
            Ok(r) => r.exit_code == 0,
            Err(_) => false,
        }
    }

    /// Return true if the session's sandbox currently has a running AI CLI
    /// process (claude / codex / opencode) — either attached to a tmux
    /// client or running detached in the background. Used by the idle
    /// sweeper to avoid reaping sessions that are doing autonomous work
    /// between user keystrokes.
    ///
    /// Checks via `pgrep` inside the container. A missing `pgrep` (or any
    /// exec failure) returns false, which is the safe default — the
    /// sweeper will only *skip* reaping when we can positively confirm
    /// work is happening.
    pub async fn has_ai_cli_running(&self, session_id: i32) -> bool {
        let sessions = self.sessions.read().await;
        let Some(live) = sessions.get(&session_id) else {
            return false;
        };
        match self
            .provider
            .exec(
                &live.handle,
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    // -f matches the full command line, so `claude` running
                    // as `node .../claude` still matches.
                    "pgrep -f '(^|/)claude( |$)|(^|/)codex( |$)|(^|/)opencode( |$)' >/dev/null"
                        .to_string(),
                ],
                HashMap::new(),
                None,
            )
            .await
        {
            Ok(r) => r.exit_code == 0,
            Err(_) => false,
        }
    }

    /// Attempt to adopt an existing sandbox container for a session after
    /// server restart. Returns true if adoption succeeded (container was
    /// found alive and registered into the in-memory map).
    pub async fn adopt_existing(
        &self,
        session_id: i32,
        project_id: i32,
    ) -> Result<bool, WorkspaceError> {
        // Already tracked? Nothing to do.
        if self.sessions.read().await.contains_key(&session_id) {
            return Ok(true);
        }

        match self.provider.recover(session_id).await {
            Ok(Some(handle)) => {
                let work_dir = handle.work_dir.clone();
                // Adopted sessions are NOT first-message — there's prior
                // conversation in the sandbox's claude jsonl.
                // Reconstruct the deterministic host work dir used at create
                // time (see message_executor.rs: `temp_dir/workspace-{id}`).
                // This is the same bind-mount source the sandbox was created
                // with, so paste-image can write through it directly.
                let host_work_dir_guess =
                    std::env::temp_dir().join(format!("workspace-{}", session_id));
                let host_work_dir = if host_work_dir_guess.exists() {
                    Some(host_work_dir_guess)
                } else {
                    None
                };
                let live = LiveSession {
                    session_id,
                    project_id,
                    handle,
                    work_dir,
                    host_work_dir,
                    is_first_message: false,
                };
                self.sessions.write().await.insert(session_id, live);
                tracing::info!(
                    "Adopted existing sandbox container for session {}",
                    session_id
                );
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(WorkspaceError::SandboxCreationFailed {
                session_id,
                reason: format!("adopt_existing failed: {}", e),
            }),
        }
    }

    /// Build the Claude CLI command for a workspace message.
    pub fn build_chat_cmd(
        &self,
        prompt: &str,
        max_turns: i32,
        continue_conversation: bool,
        provider: &str,
        model: Option<&str>,
    ) -> Vec<String> {
        // Reuse the existing build_claude_cmd from temps-agents executor
        match provider {
            "claude_cli" | "" => {
                let mut cmd = vec!["claude".to_string(), "--print".to_string()];
                if continue_conversation {
                    cmd.push("--continue".to_string());
                }
                cmd.push(prompt.to_string());
                cmd.extend([
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--max-turns".to_string(),
                    max_turns.to_string(),
                    "--dangerously-skip-permissions".to_string(),
                    "--verbose".to_string(),
                ]);
                if let Some(m) = model {
                    if !m.is_empty() {
                        cmd.push("--model".to_string());
                        cmd.push(m.to_string());
                    }
                }
                cmd
            }
            "codex_cli" => {
                let mut cmd = vec!["codex".to_string(), "exec".to_string()];
                if let Some(m) = model {
                    if !m.is_empty() {
                        cmd.push("--model".to_string());
                        cmd.push(m.to_string());
                    }
                }
                cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                cmd.push("--json".to_string());
                cmd.push(prompt.to_string());
                cmd
            }
            "opencode" => {
                // OpenCode's `run [message..]` does an lstat on the first
                // positional arg (checking if it's a directory). Long agent
                // prompts exceed NAME_MAX (255 bytes), causing ENAMETOOLONG.
                // Workaround: write the prompt to /tmp/.temps-prompt (done by
                // run_claude_once) and read it via `$(cat ...)` in a shell.
                let mut parts = vec!["opencode run".to_string()];
                if let Some(m) = model {
                    if !m.is_empty() {
                        // SAFETY: `m` is user-controllable (`ai_model` from
                        // start-session). Splicing it into a `bash -lc` string
                        // without escaping allows arbitrary command execution
                        // inside the sandbox container — see the v0.0.8
                        // security audit. Always single-quote escape.
                        parts.push(format!("--model {}", posix_shell_escape(m)));
                    }
                }
                if continue_conversation {
                    parts.push("--continue".to_string());
                }
                parts.push("--format json".to_string());
                parts.push("\"$(cat /tmp/.temps-prompt)\"".to_string());
                vec!["bash".to_string(), "-lc".to_string(), parts.join(" ")]
            }
            // Reject unknown providers rather than executing the raw string
            // as a binary inside the sandbox. `create_session` validates
            // against the catalog allowlist, so this branch is defense in
            // depth — if we get here, fall back to claude_cli with the bare
            // prompt and log loudly.
            other => {
                tracing::error!(
                    "build_chat_cmd: unknown ai_provider {:?} reached the executor — \
                     rejecting and falling back to claude_cli (this indicates a \
                     missing allowlist check upstream)",
                    other
                );
                let mut cmd = vec!["claude".to_string(), "--print".to_string()];
                if continue_conversation {
                    cmd.push("--continue".to_string());
                }
                cmd.push(prompt.to_string());
                cmd.extend([
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--max-turns".to_string(),
                    max_turns.to_string(),
                    "--dangerously-skip-permissions".to_string(),
                    "--verbose".to_string(),
                ]);
                cmd
            }
        }
    }

    /// Mark a session's first message as sent (subsequent messages use --continue).
    pub async fn mark_first_message_sent(&self, session_id: i32) {
        let mut sessions = self.sessions.write().await;
        if let Some(live) = sessions.get_mut(&session_id) {
            live.is_first_message = false;
        }
    }

    /// Check if this is the first message in a session (determines --continue flag).
    pub async fn is_first_message(&self, session_id: i32) -> bool {
        let sessions = self.sessions.read().await;
        sessions
            .get(&session_id)
            .map(|s| s.is_first_message)
            .unwrap_or(true)
    }

    /// Check if a session's sandbox is alive.
    ///
    /// Returns true only if BOTH:
    ///   1. The session is tracked in the in-memory map, AND
    ///   2. The provider confirms the underlying container is actually running.
    ///
    /// This avoids the "stale in-memory handle" failure where we think a
    /// sandbox is alive but its container was stopped/removed out of band.
    pub async fn is_alive(&self, session_id: i32) -> bool {
        let sessions = self.sessions.read().await;
        if let Some(live) = sessions.get(&session_id) {
            self.provider.is_alive(&live.handle).await.unwrap_or(false)
        } else {
            false
        }
    }

    /// Stop a session's sandbox container without removing it.
    /// The session remains tracked so it can be started again later.
    pub async fn stop_sandbox(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;
        self.provider
            .stop(&live.handle)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to stop sandbox: {}", e),
            })
    }

    /// Start a previously stopped sandbox container.
    pub async fn start_sandbox(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;
        self.provider
            .start(&live.handle)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to start sandbox: {}", e),
            })
    }

    /// Restart a session's sandbox in place. The container ID is preserved
    /// so any inbound preview requests keep working as soon as the dev
    /// server is back up inside the container.
    pub async fn restart_sandbox(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let sessions = self.sessions.read().await;
        let live = sessions
            .get(&session_id)
            .ok_or(WorkspaceError::SandboxNotAvailable { session_id })?;
        self.provider
            .restart(&live.handle)
            .await
            .map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("Failed to restart sandbox: {}", e),
            })
    }

    /// Release (destroy) a session's sandbox and remove from tracking.
    ///
    /// `purge_volumes` controls whether the per-session `/home/temps`
    /// named volume is also deleted. Pass `false` on session *close* so
    /// the user's claude auth, shell history, and tmux state survive the
    /// next reopen; pass `true` on session *delete* so nothing leaks.
    pub async fn release(
        &self,
        session_id: i32,
        purge_volumes: bool,
    ) -> Result<(), WorkspaceError> {
        let live = self.sessions.write().await.remove(&session_id);

        if let Some(live) = live {
            self.provider
                .destroy(&live.handle, purge_volumes)
                .await
                .map_err(|e| WorkspaceError::AiCliFailed {
                    session_id,
                    reason: format!("Failed to destroy sandbox: {}", e),
                })?;
            tracing::info!(
                "Released sandbox for workspace session {} (purge_volumes={})",
                session_id,
                purge_volumes
            );
        }

        Ok(())
    }

    /// Get the sandbox handle for a session (if active).
    pub async fn get_handle(&self, session_id: i32) -> Option<SandboxHandle> {
        self.sessions
            .read()
            .await
            .get(&session_id)
            .map(|s| s.handle.clone())
    }

    /// Get the host-side work dir bind-mounted into this session's sandbox.
    /// Returns None for sessions recovered from disk (pre-restart), since the
    /// path wasn't persisted. Callers can fall back to Docker `inspect` on
    /// the container to rehydrate it in that case.
    pub async fn get_host_work_dir(&self, session_id: i32) -> Option<PathBuf> {
        self.sessions
            .read()
            .await
            .get(&session_id)
            .and_then(|s| s.host_work_dir.clone())
    }

    /// Get all active session IDs (for idle timeout checking).
    pub async fn active_session_ids(&self) -> Vec<i32> {
        self.sessions.read().await.keys().copied().collect()
    }

    /// Build environment variables for a workspace sandbox.
    ///
    /// Static credentials (injected at container creation):
    /// - TEMPS_API_TOKEN — project-scoped deployment token
    /// - TEMPS_API_URL — Temps instance URL
    /// - TEMPS_PROJECT_ID — for the memory script and any other scoped CLI calls
    /// - TEMPS_WORKFLOW_SLUG — for the memory script
    /// - AI provider env var (varies by provider: `ANTHROPIC_API_KEY`,
    ///   `OPENAI_API_KEY`, …) — only set for `ApiKey` flavors. File-based
    ///   flavors (Claude subscription, OpenCode config) leave the env empty
    ///   and rely on `seed_provider_credentials` writing the credential file
    ///   inside the container after creation.
    /// - PATH — extended with /home/temps/.temps/bin so `memory` is on PATH
    ///
    /// Service credentials (DATABASE_URL, REDIS_URL) are NOT baked in.
    /// They are fetched at runtime via `temps services connect <name>`.
    pub fn build_env_vars(
        temps_api_url: &str,
        temps_api_token: &str,
        provider_id: &str,
        auth_type: &str,
        decrypted_credential: Option<&[u8]>,
    ) -> HashMap<String, String> {
        Self::build_env_vars_with_workflow(
            temps_api_url,
            temps_api_token,
            provider_id,
            auth_type,
            decrypted_credential,
            None,
            None,
        )
    }

    /// Like `build_env_vars` but also injects `TEMPS_PROJECT_ID` and
    /// `TEMPS_WORKFLOW_SLUG` so the memory script knows which workflow to
    /// scope its writes to.
    pub fn build_env_vars_with_workflow(
        temps_api_url: &str,
        temps_api_token: &str,
        provider_id: &str,
        auth_type: &str,
        decrypted_credential: Option<&[u8]>,
        project_id: Option<i32>,
        workflow_slug: Option<&str>,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();

        env.insert("TEMPS_API_URL".to_string(), temps_api_url.to_string());
        env.insert("TEMPS_API_TOKEN".to_string(), temps_api_token.to_string());

        if let Some(pid) = project_id {
            env.insert("TEMPS_PROJECT_ID".to_string(), pid.to_string());
        }
        if let Some(slug) = workflow_slug {
            env.insert("TEMPS_WORKFLOW_SLUG".to_string(), slug.to_string());
        }

        // Catalog-driven AI credential injection. We only set an env var here
        // for `ApiKey` flavors — file-based flavors are seeded later via
        // `seed_provider_credentials` so the CLI gets full auth context (e.g.
        // Claude OAuth scopes, OpenCode auth.json) instead of just a token.
        if let (Some(creds), Some(provider)) = (decrypted_credential, find_provider(provider_id)) {
            if let Some(flavor) = provider.flavor(auth_type) {
                if matches!(flavor.format, CredentialFormat::ApiKey) {
                    if let Ok(value) = std::str::from_utf8(creds) {
                        env.insert(flavor.env_var.to_string(), value.to_string());
                    } else {
                        tracing::warn!(
                            "build_env_vars: {} credential is not valid UTF-8, skipping",
                            provider_id
                        );
                    }
                }
            } else {
                tracing::warn!(
                    "build_env_vars: provider {} has no flavor {}",
                    provider_id,
                    auth_type
                );
            }
        }

        // Tell Claude CLI to accept non-interactive mode. Harmless for other
        // providers — they ignore unknown env vars.
        env.insert("CLAUDE_CODE_ENTRYPOINT".to_string(), "cli".to_string());

        // Put /home/temps/.temps/bin on PATH so the memory script is callable
        // as `memory` from anywhere (instead of by full path). `/home/temps`
        // is the private named volume, NOT the bind-mounted `/workspace` repo,
        // so scripts installed here don't leak into the user's PR diff.
        // Also include the AI CLI install locations baked into the sandbox image:
        //   - /home/temps/.local/bin   → claude
        //   - /home/temps/.bun/bin     → codex (installed via `bun add -g`)
        //   - /home/temps/.opencode/bin → opencode (installer hardcodes this path)
        // Docker's `Config.Env` replaces the image's `ENV PATH=...` entirely,
        // so we must re-include these here or non-interactive execs can't find
        // the CLIs even though they exist on disk.
        env.insert(
            "PATH".to_string(),
            format!(
                "{home}/.temps/bin:{home}/.local/bin:{home}/.bun/bin:{home}/.opencode/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                home = SANDBOX_HOME
            ),
        );

        env
    }

    /// Write every bundled Temps skill into the sandbox's workspace. This
    /// teaches the AI how to use the Temps CLI, deploy apps, wire up SDKs,
    /// author plugins, etc.
    ///
    /// Each CLI reads skills from a different directory, so we route the
    /// skills to whichever provider is active for this session:
    /// - claude/opencode: `/home/temps/.claude/skills/<name>/SKILL.md`
    /// - codex: `/home/temps/.codex/skills/<name>/SKILL.md`
    ///
    /// We deliberately use the user's home for claude rather than the
    /// `/workspace/.claude` project-local path, because `/workspace` is a
    /// bind mount from the host repo and writes there would show up as
    /// modified files in the user's git working tree.
    ///
    /// Claude's skill discovery requires the directory (or filename) to match
    /// the frontmatter `name:` field, so the directory name comes straight
    /// from [`BUNDLED_SKILLS`] rather than being derived from disk paths.
    pub async fn inject_skill_file(
        &self,
        session_id: i32,
        ai_provider: &str,
    ) -> Result<(), WorkspaceError> {
        // All providers now use a user-level path to avoid polluting the
        // /workspace bind mount with Temps-injected skill files.
        let skills_base = match ai_provider {
            "codex_cli" => format!("{}/.codex/skills", SANDBOX_HOME),
            _ => format!("{}/.claude/skills", SANDBOX_HOME),
        };

        // Remove the stale flat-file version from older sandbox builds.
        // We deliberately do NOT touch the work-dir's `.claude/skills`
        // directory beyond this: the work dir is the user's repo and any
        // skills they keep there belong to them.
        let cleanup_cmd = vec![
            "rm".to_string(),
            "-f".to_string(),
            format!("{}/.claude/skills/temps-platform.md", SANDBOX_WORK_DIR),
        ];
        let _ = self
            .exec(session_id, cleanup_cmd, HashMap::new(), None)
            .await;

        for (name, body) in BUNDLED_SKILLS {
            let skill_dir = format!("{}/{}", skills_base, name);
            let skill_path = format!("{}/SKILL.md", skill_dir);

            let mkdir_cmd = vec!["mkdir".to_string(), "-p".to_string(), skill_dir.clone()];
            self.exec(session_id, mkdir_cmd, HashMap::new(), None)
                .await?;

            // Write via native tar upload (avoids the bollard exec
            // phantom-stream hang on silent heredoc writes).
            self.write_file(session_id, &skill_path, body.as_bytes(), 0o644)
                .await?;

            tracing::debug!(
                "Injected Temps skill '{}' into session {} at {}",
                name,
                session_id,
                skill_path
            );
        }

        Ok(())
    }

    /// Seed `~/.claude.json` inside the sandbox so Claude CLI skips the
    /// one-time onboarding flow (theme picker, "Let's get started" screen)
    /// on first launch. Each workspace session gets its own `/home/temps`
    /// named volume, so without this seed every new session's first
    /// `claude` invocation in the terminal would block on the theme picker
    /// — even though the user is already authenticated via
    /// `~/.claude/.credentials.json`.
    ///
    /// Best-effort: if the file already exists (e.g. the user completed
    /// onboarding once and the home volume persisted it across a container
    /// restart), we leave it alone.
    pub async fn seed_claude_config(&self, session_id: i32) -> Result<(), WorkspaceError> {
        // Merge with any existing file rather than clobbering. The sandbox
        // injector writes `mcpServers` into /home/temps/.claude.json earlier
        // in session bootstrap; if we overwrite blindly we'd strip those.
        // `projects["/workspace"].hasTrustDialogAccepted = true` suppresses the
        // "Quick safety check: is this a project you trust?" prompt, and
        // `hasCompletedProjectOnboarding` covers the "Welcome back!" variant.
        let claude_json_path = format!("{}/.claude.json", SANDBOX_HOME);
        let claude_dir = format!("{}/.claude", SANDBOX_HOME);
        let claude_settings_path = format!("{}/.claude/settings.json", SANDBOX_HOME);

        let existing = self
            .read_file(session_id, &claude_json_path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let mut body = existing.unwrap_or_else(|| serde_json::json!({}));

        // Onboarding keys — safe to always set/overwrite.
        body["numStartups"] = serde_json::json!(3);
        body["installMethod"] = serde_json::json!("native");
        body["autoUpdates"] = serde_json::json!(false);
        body["tipsHistory"] = serde_json::json!({ "new-user-warmup": 2 });
        body["autoUpdatesProtectedForNative"] = serde_json::json!(true);
        body["hasCompletedOnboarding"] = serde_json::json!(true);
        body["lastOnboardingVersion"] = serde_json::json!("2.1.96");
        body["voiceNoticeSeenCount"] = serde_json::json!(2);
        body["cachedExtraUsageDisabledReason"] = serde_json::json!(null);
        body["officialMarketplaceAutoInstallAttempted"] = serde_json::json!(true);
        body["officialMarketplaceAutoInstalled"] = serde_json::json!(true);
        body["theme"] = serde_json::json!("dark");
        body["bypassPermissionsModeAccepted"] = serde_json::json!(true);
        body["hasSeenWelcome"] = serde_json::json!(true);

        // Merge /workspace trust into `projects` without stomping sibling
        // entries the injector or the CLI may have added.
        let projects = body
            .get_mut("projects")
            .and_then(|v| v.as_object_mut())
            .map(|_| ())
            .is_some();
        if !projects {
            body["projects"] = serde_json::json!({});
        }
        // `dontCrawlDirectory` + `hasTrustDialogAccepted` + the two
        // `*BypassPermissionsModeAccepted` flags together silence the
        // "WARNING: Claude Code running in Bypass Permissions mode" gate.
        // Claude CLI checks per-project first, then falls back to the root-
        // level `bypassPermissionsModeAccepted` — we set both to be safe.
        body["projects"][SANDBOX_WORK_DIR] = serde_json::json!({
            "hasTrustDialogAccepted": true,
            "hasCompletedProjectOnboarding": true,
            "projectOnboardingSeenCount": 1,
            "dontCrawlDirectory": false,
            "hasClaudeMdExternalIncludesApproved": true,
            "hasClaudeMdExternalIncludesWarningShown": true,
            "bypassPermissionsModeAccepted": true,
            "allowedTools": [],
            "history": [],
        });

        let bytes = serde_json::to_vec_pretty(&body).map_err(|e| WorkspaceError::AiCliFailed {
            session_id,
            reason: format!("seed_claude_config: serialize failed: {}", e),
        })?;

        self.write_file(session_id, &claude_json_path, &bytes, 0o600)
            .await?;

        tracing::debug!(
            "Seeded {} for session {} (merged; preserved mcpServers if present)",
            claude_json_path,
            session_id
        );

        // Also seed ~/.claude/settings.json — again merging so we don't lose
        // anything the injector may have put there in the future.
        self.exec(
            session_id,
            vec!["mkdir".into(), "-p".into(), claude_dir.clone()],
            std::collections::HashMap::new(),
            None,
        )
        .await?;

        let existing_settings = self
            .read_file(session_id, &claude_settings_path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let mut settings = existing_settings.unwrap_or_else(|| serde_json::json!({}));
        settings["theme"] = serde_json::json!("dark");
        // `effort: "medium"` suppresses the "We recommend medium effort for
        // Opus" picker on first launch. Users can still override per-turn
        // with `/effort high` or `ultrathink`.
        settings["effort"] = serde_json::json!("medium");

        let settings_bytes =
            serde_json::to_vec_pretty(&settings).map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("seed_claude_config: settings serialize failed: {}", e),
            })?;
        self.write_file(session_id, &claude_settings_path, &settings_bytes, 0o600)
            .await?;

        Ok(())
    }

    /// Write `/home/temps/.claude/.credentials.json` with the OAuth token so
    /// Claude CLI authenticates without needing `CLAUDE_CODE_OAUTH_TOKEN` env
    /// var. This gives the CLI the full auth context (subscription type, scopes)
    /// rather than just a bare token.
    ///
    /// For `api_key` auth type, does nothing — `ANTHROPIC_API_KEY` in `~/.env`
    /// is the correct mechanism for API key auth.
    pub async fn seed_claude_credentials(
        &self,
        session_id: i32,
        access_token: &str,
        auth_type: &str,
    ) -> Result<(), WorkspaceError> {
        if auth_type != "subscription" {
            return Ok(());
        }

        // Ensure ~/.claude/ directory exists
        self.exec(
            session_id,
            vec![
                "mkdir".into(),
                "-p".into(),
                format!("{}/.claude", SANDBOX_HOME),
            ],
            std::collections::HashMap::new(),
            None,
        )
        .await?;

        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": access_token,
                "expiresAt": oauth_expires_at_ms(),
                "scopes": [
                    "user:inference",
                    "user:mcp_servers",
                    "user:profile",
                    "user:sessions:claude_code"
                ],
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_20x"
            }
        });
        let bytes = serde_json::to_vec_pretty(&body).map_err(|e| WorkspaceError::AiCliFailed {
            session_id,
            reason: format!("seed_claude_credentials: serialize failed: {}", e),
        })?;

        let creds_path = format!("{}/.claude/.credentials.json", SANDBOX_HOME);
        self.write_file(session_id, &creds_path, &bytes, 0o600)
            .await?;

        tracing::debug!("Seeded {} for session {}", creds_path, session_id);
        Ok(())
    }

    /// Seed `/home/temps/.codex/config.toml` so Codex CLI skips its first-run
    /// "Do you trust the contents of this directory?" prompt for `/workspace`.
    /// Without this, the very first `codex` launch inside a fresh sandbox
    /// blocks on an interactive 1/2 picker that the PTY has no way to answer
    /// automatically — the user sees the prompt but the CLI never proceeds.
    ///
    /// Merge strategy: if the file already contains a `[projects."/workspace"]`
    /// section (e.g. the user edited it in a previous session and the home
    /// volume persisted it), leave it alone. Otherwise append the trust
    /// section to whatever is there (preserving `model = "..."`,
    /// `model_reasoning_effort = "..."`, and any other top-level keys the
    /// installer or the user may have added).
    ///
    /// Best-effort: a failure here shouldn't abort sandbox creation, so the
    /// caller logs a warning and moves on.
    pub async fn seed_codex_config(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let codex_dir = format!("{}/.codex", SANDBOX_HOME);
        let codex_config_path = format!("{}/.codex/config.toml", SANDBOX_HOME);
        let trust_header = format!("[projects.\"{}\"]", SANDBOX_WORK_DIR);

        self.exec(
            session_id,
            vec!["mkdir".into(), "-p".into(), codex_dir],
            std::collections::HashMap::new(),
            None,
        )
        .await?;

        let existing = self
            .read_file(session_id, &codex_config_path)
            .await
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();

        // Already trusts the work dir → nothing to do.
        if existing.contains(&trust_header) {
            tracing::debug!(
                "Codex config.toml already trusts {} for session {} — skipping seed",
                SANDBOX_WORK_DIR,
                session_id
            );
            return Ok(());
        }

        // Append the trust section. Codex's config.toml format is one
        // `[projects."<abs-path>"]` table per trusted directory, with a single
        // `trust_level = "trusted"` key underneath. Leading newline keeps us
        // safe when `existing` doesn't already end with one.
        let trust_block = format!("\n{trust_header}\ntrust_level = \"trusted\"\n");
        let mut body = existing;
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&trust_block);

        self.write_file(session_id, &codex_config_path, body.as_bytes(), 0o600)
            .await?;

        tracing::debug!(
            "Seeded {} with {} trust for session {}",
            codex_config_path,
            SANDBOX_WORK_DIR,
            session_id
        );
        Ok(())
    }

    /// Generic per-provider credential seeder. Dispatches off the catalog so
    /// that adding a new AI CLI (Gemini, Grok, …) only requires a catalog
    /// entry — no new method, no new arm in `message_executor`.
    ///
    /// Returns the env vars that should be merged into the session's `.env`
    /// file (for `ApiKey` flavors). For file-based flavors the credential is
    /// written directly to disk inside the sandbox and an empty map is
    /// returned.
    ///
    /// `decrypted_credential` is the plaintext bytes (already decrypted by
    /// the caller). For `OauthToken` flavors the bytes are the OAuth access
    /// token; we wrap them in Claude's expected JSON envelope before writing.
    /// For `ConfigFile` flavors the bytes are the raw file body.
    pub async fn seed_provider_credentials(
        &self,
        session_id: i32,
        provider_id: &str,
        auth_type: &str,
        decrypted_credential: &[u8],
    ) -> Result<HashMap<String, String>, WorkspaceError> {
        let provider = find_provider(provider_id).ok_or_else(|| WorkspaceError::Validation {
            message: format!(
                "seed_provider_credentials: unknown provider '{}'",
                provider_id
            ),
        })?;
        let flavor = provider
            .flavor(auth_type)
            .ok_or_else(|| WorkspaceError::Validation {
                message: format!(
                    "seed_provider_credentials: provider '{}' does not support auth_type '{}'",
                    provider_id, auth_type
                ),
            })?;

        let mut env = HashMap::new();

        match flavor.format {
            CredentialFormat::ApiKey => {
                let value = std::str::from_utf8(decrypted_credential).map_err(|e| {
                    WorkspaceError::Validation {
                        message: format!(
                            "seed_provider_credentials: {} credential is not valid UTF-8: {}",
                            provider_id, e
                        ),
                    }
                })?;
                env.insert(flavor.env_var.to_string(), value.to_string());
                tracing::debug!(
                    "Prepared {} env var for {} on session {}",
                    flavor.env_var,
                    provider_id,
                    session_id
                );
            }
            CredentialFormat::OauthToken => {
                let token = std::str::from_utf8(decrypted_credential).map_err(|e| {
                    WorkspaceError::Validation {
                        message: format!(
                            "seed_provider_credentials: {} OAuth token is not valid UTF-8: {}",
                            provider_id, e
                        ),
                    }
                })?;
                self.write_oauth_credential_file(session_id, &flavor.seed_path(), token)
                    .await?;
            }
            CredentialFormat::ConfigFile => {
                self.write_config_credential_file(
                    session_id,
                    &flavor.seed_path(),
                    decrypted_credential,
                )
                .await?;
            }
        }

        Ok(env)
    }

    /// Write Claude's OAuth credential envelope. Currently hardcoded to the
    /// `claudeAiOauth` shape since Claude is the only provider using
    /// `OauthToken`; if a second provider needs OAuth we'll teach the
    /// catalog about envelope shape.
    async fn write_oauth_credential_file(
        &self,
        session_id: i32,
        seed_path: &str,
        access_token: &str,
    ) -> Result<(), WorkspaceError> {
        // Make sure the parent directory exists. Splitting on '/' is fine
        // because catalog seed paths are resolved against SANDBOX_HOME and
        // use Unix slashes.
        if let Some(idx) = seed_path.rfind('/') {
            let parent = &seed_path[..idx];
            self.exec(
                session_id,
                vec!["mkdir".into(), "-p".into(), parent.to_string()],
                std::collections::HashMap::new(),
                None,
            )
            .await?;
        }

        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": access_token,
                "expiresAt": oauth_expires_at_ms(),
                "scopes": [
                    "user:inference",
                    "user:mcp_servers",
                    "user:profile",
                    "user:sessions:claude_code"
                ],
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_20x"
            }
        });
        let bytes = serde_json::to_vec_pretty(&body).map_err(|e| WorkspaceError::AiCliFailed {
            session_id,
            reason: format!(
                "seed_provider_credentials: oauth envelope serialize failed: {}",
                e
            ),
        })?;

        self.write_file(session_id, seed_path, &bytes, 0o600)
            .await?;
        tracing::debug!(
            "Seeded OAuth credential file {} for session {}",
            seed_path,
            session_id
        );
        Ok(())
    }

    /// Write a raw config-file credential (e.g. OpenCode's `auth.json`)
    /// verbatim to the catalog-declared seed path. Caller is responsible for
    /// supplying valid file content — we just persist it.
    async fn write_config_credential_file(
        &self,
        session_id: i32,
        seed_path: &str,
        bytes: &[u8],
    ) -> Result<(), WorkspaceError> {
        if let Some(idx) = seed_path.rfind('/') {
            let parent = &seed_path[..idx];
            self.exec(
                session_id,
                vec!["mkdir".into(), "-p".into(), parent.to_string()],
                std::collections::HashMap::new(),
                None,
            )
            .await?;
        }
        self.write_file(session_id, seed_path, bytes, 0o600).await?;
        tracing::debug!(
            "Seeded config credential file {} ({} bytes) for session {}",
            seed_path,
            bytes.len(),
            session_id
        );
        Ok(())
    }

    /// Write `/home/temps/.env` inside the sandbox containing the given key/value
    /// pairs and install a global `~/.claude/CLAUDE.md` that instructs Claude
    /// to source it before running commands. Tokens (git providers, linked
    /// services, etc.) are stored here so they can be refreshed by simply
    /// rewriting the file — no container restart required.
    ///
    /// Values are written using a single-quoted shell-safe encoding so that
    /// special characters in tokens don't break sourcing.
    pub async fn inject_env_file(
        &self,
        session_id: i32,
        env: &HashMap<String, String>,
        project_context: Option<&ProjectContext<'_>>,
    ) -> Result<(), WorkspaceError> {
        let body = build_env_file_body(env);

        // Native tar upload (mode 0o600 — secrets). Path is resolved against
        // the sandbox user's HOME (see `sandbox::user::SANDBOX_HOME`).
        let env_path = format!("{}/.env", SANDBOX_HOME);
        self.write_file(session_id, &env_path, body.as_bytes(), 0o600)
            .await?;

        // Global CLAUDE.md telling the agent (a) which Temps project this
        // sandbox belongs to, and (b) how to use the managed env file. This
        // lives in `~/.claude/CLAUDE.md` so it loads for every session,
        // independent of the project's own CLAUDE.md.
        let project_section = match project_context {
            Some(ctx) => format!(
                r#"# Current Temps project

This sandbox belongs to a single Temps project. Use these values for any
`temps-cli` / `bunx @temps-sdk/cli` command — DO NOT ask the user which
project to operate on:

- **Project ID:** `{id}`
- **Slug:** `{slug}`
- **Name:** {name}
- **Repository:** `{repo_owner}/{repo_name}`
- **Default branch:** `{branch}`

When invoking the Temps CLI, always pass `--project {slug}` (or the
equivalent project flag) so commands are scoped to this project.

"#,
                id = ctx.id,
                slug = ctx.slug,
                name = ctx.name,
                repo_owner = ctx.repo_owner,
                repo_name = ctx.repo_name,
                branch = ctx.branch,
            ),
            None => String::new(),
        };

        let claude_md = format!(
            r#"{project_section}# Sandbox environment

This sandbox is managed by Temps. Credentials for linked services and git
providers are stored in `~/.env` and refreshed in-place by the platform — they
may rotate at any time.

**Before running any shell command that needs credentials**, source the env
file in the same command:

```bash
. ~/.env && <your command>
```

Examples:

```bash
. ~/.env && gh pr list
. ~/.env && glab mr list
. ~/.env && psql "$DATABASE_URL" -c '\dt'
```

Do not copy values out of `~/.env` into other files, scripts, or commit
messages — they are short-lived and may rotate. Always re-read from `~/.env`.
"#,
            project_section = project_section
        );
        let claude_md = claude_md.as_str();

        let claude_md_path = format!("{}/.claude/CLAUDE.md", SANDBOX_HOME);
        self.write_file(session_id, &claude_md_path, claude_md.as_bytes(), 0o644)
            .await?;

        tracing::debug!(
            "Injected ~/.env ({} keys) and global CLAUDE.md into session {}",
            env.len(),
            session_id
        );
        Ok(())
    }

    /// Install the memory CLI script (`/home/temps/.temps/bin/memory`) in the
    /// sandbox so the AI can read/write workflow memory via simple shell commands.
    /// The script uses curl to call the Temps API; no CLI binary required.
    pub async fn inject_memory_script(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let cmd = crate::services::memory_script::install_command();
        self.exec(session_id, cmd, HashMap::new(), None).await?;
        tracing::debug!("Installed memory script in session {}", session_id);
        Ok(())
    }

    /// Attempt to recover a sandbox after server restart.
    pub async fn recover_session(
        &self,
        session_id: i32,
        project_id: i32,
    ) -> Result<bool, WorkspaceError> {
        match self.provider.recover(session_id).await {
            Ok(Some(handle)) => {
                // Reconstruct the deterministic host bind-mount source so
                // paste-image works without needing a new message first.
                let host_work_dir_guess =
                    std::env::temp_dir().join(format!("workspace-{}", session_id));
                let host_work_dir = if host_work_dir_guess.exists() {
                    Some(host_work_dir_guess)
                } else {
                    None
                };
                let live = LiveSession {
                    session_id,
                    project_id,
                    handle,
                    work_dir: PathBuf::from("/workspace"),
                    host_work_dir,
                    is_first_message: false, // Recovered sessions have prior context
                };
                self.sessions.write().await.insert(session_id, live);
                tracing::info!("Recovered sandbox for workspace session {}", session_id);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => {
                tracing::warn!(
                    "Failed to recover sandbox for session {}: {}",
                    session_id,
                    e
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use temps_agents::error::AgentError;

    /// A fake sandbox provider for unit testing.
    struct FakeSandboxProvider {
        should_fail: bool,
    }

    #[async_trait::async_trait]
    impl SandboxProvider for FakeSandboxProvider {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            if self.should_fail {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "fake".to_string(),
                    reason: "forced failure".to_string(),
                });
            }
            Ok(SandboxHandle {
                sandbox_id: format!("fake-{}", config.run_id),
                sandbox_name: format!("temps-sandbox-{}", config.run_id),
                work_dir: PathBuf::from("/workspace"),
            })
        }

        async fn exec(
            &self,
            _handle: &SandboxHandle,
            cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            Ok(SandboxExecResult {
                exit_code: 0,
                stdout: format!("executed: {:?}", cmd),
                stderr: String::new(),
            })
        }

        async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(true)
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(Vec::new())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn destroy(
            &self,
            _handle: &SandboxHandle,
            _purge_volumes: bool,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn recover(&self, run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(Some(SandboxHandle {
                sandbox_id: format!("recovered-{}", run_id),
                sandbox_name: format!("temps-sandbox-{}", run_id),
                work_dir: PathBuf::from("/workspace"),
            }))
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".to_string()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".to_string())
        }
    }

    fn make_manager(should_fail: bool) -> WorkspaceSessionManager {
        let provider = Arc::new(FakeSandboxProvider { should_fail });
        let encryption =
            Arc::new(EncryptionService::new("test-key-32-bytes-long-padding!!").unwrap());
        WorkspaceSessionManager::new(provider, encryption, Duration::from_secs(1800))
    }

    #[tokio::test]
    async fn test_create_sandbox_success() {
        let manager = make_manager(false);
        let result = manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_ok());
        let handle = result.unwrap();
        assert_eq!(handle.sandbox_id, "fake-1");
    }

    #[tokio::test]
    async fn test_create_sandbox_failure() {
        let manager = make_manager(true);
        let result = manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            WorkspaceError::SandboxCreationFailed { session_id: 1, .. }
        ));
    }

    #[tokio::test]
    async fn test_exec_success() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let result = manager
            .exec(
                1,
                vec!["echo".to_string(), "hello".to_string()],
                HashMap::new(),
                None,
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().exit_code, 0);
    }

    #[tokio::test]
    async fn test_exec_no_sandbox_fails() {
        let manager = make_manager(false);

        let result = manager
            .exec(999, vec!["echo".to_string()], HashMap::new(), None)
            .await;

        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(matches!(
            err,
            WorkspaceError::SandboxNotAvailable { session_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_first_message_tracking() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(manager.is_first_message(1).await);
        manager.mark_first_message_sent(1).await;
        assert!(!manager.is_first_message(1).await);
    }

    #[tokio::test]
    async fn test_release_sandbox() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(manager.is_alive(1).await);
        manager.release(1, true).await.unwrap();
        assert!(!manager.is_alive(1).await);
    }

    #[tokio::test]
    async fn test_release_nonexistent_is_ok() {
        let manager = make_manager(false);
        let result = manager.release(999, true).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_active_session_ids() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000bb",
                10,
                PathBuf::from("/tmp/t1"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        manager
            .create_sandbox(
                2,
                "wss_00000000000000cc",
                10,
                PathBuf::from("/tmp/t2"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let mut ids = manager.active_session_ids().await;
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_recover_session() {
        let manager = make_manager(false);
        let recovered = manager.recover_session(42, 10).await.unwrap();
        assert!(recovered);
        assert!(manager.is_alive(42).await);
    }

    #[test]
    fn test_build_env_vars_claude_api_key() {
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "api_key",
            Some(b"sk-ant-123"),
        );

        assert_eq!(env.get("TEMPS_API_URL").unwrap(), "http://localhost:3000");
        assert_eq!(env.get("TEMPS_API_TOKEN").unwrap(), "test-token");
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-123");
        assert!(!env.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn test_build_env_vars_codex_api_key() {
        // Codex uses OPENAI_API_KEY — confirms the catalog dispatch picks
        // the right env var name per provider rather than hardcoding ANTHROPIC.
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "codex_cli",
            "api_key",
            Some(b"sk-openai-xyz"),
        );

        assert_eq!(env.get("OPENAI_API_KEY").unwrap(), "sk-openai-xyz");
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn test_build_env_vars_subscription_uses_no_env_var() {
        // Subscription auth is OauthToken format → seeded as a file, never
        // as an env var. Neither ANTHROPIC_API_KEY nor any other key should
        // land in the container env.
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "subscription",
            Some(b"oauth-token-123"),
        );

        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn test_build_env_vars_opencode_config_file_uses_no_env_var() {
        // ConfigFile flavors are seeded directly to disk — env stays empty.
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "opencode",
            "config_file",
            Some(b"{\"oauth\": {}}"),
        );

        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn test_build_env_vars_no_ai_key() {
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "api_key",
            None,
        );

        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(env.contains_key("TEMPS_API_URL"));
    }

    #[test]
    fn test_build_env_vars_includes_path() {
        let env = WorkspaceSessionManager::build_env_vars(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "api_key",
            None,
        );
        let path = env.get("PATH").expect("PATH should be set");
        let expected_prefix = format!("{}/.temps/bin:", SANDBOX_HOME);
        assert!(
            path.starts_with(&expected_prefix),
            "memory script dir must come first in PATH (got: {})",
            path
        );
    }

    #[test]
    fn test_build_env_vars_with_workflow_includes_scope() {
        let env = WorkspaceSessionManager::build_env_vars_with_workflow(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "api_key",
            Some(b"sk-ant-xxx"),
            Some(42),
            Some("error-autofix"),
        );

        assert_eq!(env.get("TEMPS_PROJECT_ID").unwrap(), "42");
        assert_eq!(env.get("TEMPS_WORKFLOW_SLUG").unwrap(), "error-autofix");
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-xxx");
    }

    #[test]
    fn test_build_env_vars_with_workflow_omits_scope_when_none() {
        let env = WorkspaceSessionManager::build_env_vars_with_workflow(
            "http://localhost:3000",
            "test-token",
            "claude_cli",
            "api_key",
            None,
            None,
            None,
        );

        assert!(!env.contains_key("TEMPS_PROJECT_ID"));
        assert!(!env.contains_key("TEMPS_WORKFLOW_SLUG"));
    }

    #[test]
    fn env_file_body_prepends_memory_bin_to_path() {
        // The bash `memory` script must resolve as a bare command in any
        // shell that sources ~/.env. Guarded in a unit test so the next
        // contributor to refactor env injection gets immediate feedback.
        let mut env = HashMap::new();
        env.insert("TEMPS_API_URL".to_string(), "http://api".to_string());
        env.insert("TEMPS_API_TOKEN".to_string(), "tok".to_string());

        let body = build_env_file_body(&env);
        assert!(
            body.contains(&format!(
                "export PATH='{}:'\"$PATH\"",
                temps_core::MEMORY_SCRIPT_DIR
            )),
            "PATH export missing or malformed. Full body:\n{body}",
        );
        // Prepend, not append: the memory dir must come first.
        let path_line = body
            .lines()
            .find(|l| l.starts_with("export PATH="))
            .expect("PATH line missing");
        assert!(
            path_line.contains(&format!("'{}:'", temps_core::MEMORY_SCRIPT_DIR)),
            "memory bin dir must be prepended, not appended: {path_line}",
        );
    }

    #[test]
    fn env_file_body_escapes_single_quotes() {
        let mut env = HashMap::new();
        env.insert("EVIL".to_string(), "it's \"bad\"".to_string());
        let body = build_env_file_body(&env);
        assert!(body.contains("export EVIL='it'\\''s \"bad\"'"));
    }

    #[test]
    fn env_file_body_is_deterministic() {
        // Refreshes happen often; identical input must produce identical
        // output so diffs on disk are empty-op when nothing changed.
        let mut a = HashMap::new();
        a.insert("B".to_string(), "2".to_string());
        a.insert("A".to_string(), "1".to_string());
        let mut b = HashMap::new();
        b.insert("A".to_string(), "1".to_string());
        b.insert("B".to_string(), "2".to_string());
        assert_eq!(build_env_file_body(&a), build_env_file_body(&b));
    }

    #[tokio::test]
    async fn test_inject_memory_script() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // FakeSandboxProvider returns success for any exec, so we just verify
        // the call doesn't error out.
        let result = manager.inject_memory_script(1).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_inject_skill_file() {
        let manager = make_manager(false);
        manager
            .create_sandbox(
                1,
                "wss_00000000000000aa",
                10,
                PathBuf::from("/tmp/test"),
                HashMap::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // This should succeed — FakeSandboxProvider just returns success
        let result = manager.inject_skill_file(1, "claude_cli").await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_skill_file_content() {
        // Verify the bundled skill carries the section headers and CLI
        // entry point the AI relies on. If the skill is rewritten and these
        // sections move, update the assertions to the new headers — the
        // intent is "the doc is wired up", not specific wording.
        assert!(TEMPS_PLATFORM_SKILL.contains("@temps-sdk/cli"));
        assert!(TEMPS_PLATFORM_SKILL.contains("## Analytics"));
        assert!(TEMPS_PLATFORM_SKILL.contains("## Error Tracking"));
        assert!(TEMPS_PLATFORM_SKILL.contains("## Services"));
        assert!(TEMPS_PLATFORM_SKILL.contains("## Deployments"));
        assert!(TEMPS_PLATFORM_SKILL.contains("## Monitoring"));
    }

    #[test]
    fn test_bundled_skills_complete_and_named_correctly() {
        // Every skill shipped under temps/skills/ should be in the bundle.
        // Names must match the directory and the frontmatter `name:` field —
        // Claude's skill discovery breaks if they diverge.
        let expected = [
            "temps-cli",
            "temps-platform-setup",
            "temps-mcp-setup",
            "temps-plugin",
            "deploy-to-temps",
            "add-custom-domain",
            "add-node-sdk",
            "add-react-analytics",
            "add-session-recording",
            "add-error-tracking",
        ];
        for name in expected {
            let entry = BUNDLED_SKILLS.iter().find(|(n, _)| *n == name);
            assert!(entry.is_some(), "missing bundled skill: {}", name);
            let (_, body) = entry.unwrap();
            assert!(!body.is_empty(), "skill {} is empty", name);
            // Frontmatter name must match the directory key.
            let frontmatter_line = format!("name: {}", name);
            assert!(
                body.contains(&frontmatter_line),
                "skill {} frontmatter name doesn't match bundle key",
                name
            );
        }
        assert_eq!(
            BUNDLED_SKILLS.len(),
            expected.len(),
            "unexpected extra skill"
        );
    }

    #[test]
    fn test_build_chat_cmd_claude_first_message() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("fix the bug", 25, false, "claude_cli", None);

        assert_eq!(cmd[0], "claude");
        assert_eq!(cmd[1], "--print");
        assert_eq!(cmd[2], "fix the bug");
        assert!(!cmd.contains(&"--continue".to_string()));
        assert!(cmd.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(!cmd.contains(&"--model".to_string()));
    }

    #[test]
    fn test_build_chat_cmd_claude_continue() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("follow up", 25, true, "claude_cli", None);

        assert_eq!(cmd[0], "claude");
        assert_eq!(cmd[1], "--print");
        assert_eq!(cmd[2], "--continue");
        assert_eq!(cmd[3], "follow up");
    }

    #[test]
    fn test_build_chat_cmd_claude_with_model() {
        let manager = make_manager(false);
        let cmd =
            manager.build_chat_cmd("fix it", 25, false, "claude_cli", Some("claude-sonnet-4-6"));

        assert!(cmd.contains(&"--model".to_string()));
        assert!(cmd.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn test_build_chat_cmd_codex() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("do stuff", 25, false, "codex_cli", None);

        assert_eq!(cmd[0], "codex");
        assert_eq!(cmd[1], "exec");
        assert!(cmd.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(cmd.contains(&"--json".to_string()));
    }

    #[test]
    fn test_build_chat_cmd_codex_with_model() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("do stuff", 25, false, "codex_cli", Some("gpt-5-codex"));

        assert!(cmd.contains(&"--model".to_string()));
        assert!(cmd.contains(&"gpt-5-codex".to_string()));
    }

    #[test]
    fn test_build_chat_cmd_opencode() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("help", 25, true, "opencode", None);

        // OpenCode uses a bash wrapper to avoid ENAMETOOLONG on long prompts.
        assert_eq!(cmd[0], "bash");
        assert_eq!(cmd[1], "-lc");
        let shell_cmd = &cmd[2];
        assert!(shell_cmd.contains("opencode run"));
        assert!(shell_cmd.contains("--continue"));
        assert!(shell_cmd.contains("--format json"));
        assert!(shell_cmd.contains("$(cat /tmp/.temps-prompt)"));
    }

    #[test]
    fn test_build_chat_cmd_opencode_with_model() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("help", 25, false, "opencode", Some("openai/gpt-5.4"));

        assert_eq!(cmd[0], "bash");
        let shell_cmd = &cmd[2];
        assert!(shell_cmd.contains("--model 'openai/gpt-5.4'"));
        assert!(!shell_cmd.contains("--continue"));
    }

    /// Regression test: a malicious `ai_model` value containing a single
    /// quote must not break out of the quoted argument in the `bash -lc`
    /// wrapper. With the fix, the embedded `'` is encoded as `'\''` so the
    /// payload becomes a literal substring of the model token rather than
    /// a chained shell command.
    ///
    /// We can't assert "the literal byte sequence ` touch ` is absent" —
    /// even after escaping, the payload is *inside* a single-quoted string
    /// and naturally contains those bytes. What we *can* assert is that
    /// the embedded single quote was re-encoded as `'\''`, which is the
    /// POSIX-quoted form bash interprets as a literal apostrophe rather
    /// than as a quote terminator.
    #[test]
    fn test_build_chat_cmd_opencode_model_shell_injection_is_escaped() {
        let manager = make_manager(false);
        let payload = "'; touch /tmp/pwned; echo '";
        let cmd = manager.build_chat_cmd("help", 25, false, "opencode", Some(payload));

        let shell_cmd = &cmd[2];
        // Must contain the POSIX-quoted form of every embedded single
        // quote — `'\''`. Without escaping, a bare `'` in the payload
        // would terminate the quoted argument and let the rest run as a
        // new shell statement.
        assert!(
            shell_cmd.contains("'\\''"),
            "expected single-quote escape sequence in: {shell_cmd}"
        );
        // The `--model` token must start with `''\''` (open-quote + empty
        // string + escaped apostrophe), which is the canonical POSIX form
        // for "argument that begins with an apostrophe".
        assert!(
            shell_cmd.contains("--model ''\\''"),
            "expected --model arg to start with quote+escaped-apostrophe: {shell_cmd}"
        );
        // And it must end with the closing `\'''` + space (escaped apostrophe
        // + close-quote) before the next CLI flag.
        assert!(
            shell_cmd.contains("\\''' --format json"),
            "expected --model arg to close before --format flag: {shell_cmd}"
        );
    }

    /// Regression test: an unknown `ai_provider` must not be executed as a
    /// command. The fallback now runs `claude` with the prompt, never the
    /// raw provider string.
    #[test]
    fn test_build_chat_cmd_unknown_provider_falls_back_to_claude() {
        let manager = make_manager(false);
        let cmd = manager.build_chat_cmd("hello", 25, false, "/bin/sh", None);

        assert_eq!(cmd[0], "claude");
        assert_ne!(cmd[0], "/bin/sh");
    }
}
