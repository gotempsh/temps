// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Console sign-in for subscription-backed AI CLIs.
//!
//! One [`ProviderLoginService`] owns every in-flight sign-in on this server.
//! A sign-in is a background task that drives a provider's
//! [`AiCliProvider::begin_login`] handle:
//!
//! ```text
//! start ─▶ Starting ─▶ Waiting ─(code)─▶ Verifying ─▶ Succeeded
//!                          │                 │
//!                          └── cancel / timeout / CLI exit ──▶ Cancelled | Failed
//! ```
//!
//! The CLI writes its credential into a private scratch directory. Only after
//! the CLI reports success, `verify_login` confirms it, and the access-token
//! snapshot has been saved is the scratch directory promoted to the live,
//! CLI-owned location, so a failed or cancelled attempt never damages a
//! working sign-in. The refresh token stays in that directory; the saved
//! snapshot has none (see [`crate::ai_cli::credential_snapshot`]).
//!
//! Bounds: at most `max_concurrent` sign-ins run at once, each for at most
//! `timeout`, and a process group is killed on cancel, timeout or drop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};
use utoipa::ToSchema;

use crate::ai_cli::credential_snapshot::{
    read_claude_snapshot, read_codex_snapshot, CredentialSnapshotError,
};
use crate::ai_cli::host_cli::{self, HostIdentity, ProviderLocation};
use crate::ai_cli::login::{
    validate_pasted_code, LoginChallenge, LoginError, LoginPhase, LOGIN_TIMEOUT,
    MAX_CONCURRENT_LOGINS,
};
use crate::ai_cli::AiCliProvider;

/// Who started a sign-in, for audit records written when it completes.
#[derive(Debug, Clone)]
pub struct LoginActor {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Everything a sink needs to persist a successful sign-in.
pub struct StoreLoginCredential<'a> {
    pub provider_id: &'a str,
    pub auth_type: &'a str,
    /// Access-token-only snapshot JSON; never contains a refresh token.
    pub credential: &'a str,
    pub actor: &'a LoginActor,
}

/// Persists the snapshot produced by a sign-in (encrypted settings write,
/// audit record, cache invalidation). Implemented by the host application so
/// this service stays free of settings and audit dependencies.
#[async_trait]
pub trait LoginCredentialSink: Send + Sync {
    async fn store(&self, request: StoreLoginCredential<'_>) -> Result<(), LoginError>;
}

pub type LoginProviderResolver =
    Arc<dyn Fn(&str) -> Option<Box<dyn AiCliProvider>> + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct LoginLimits {
    pub timeout: Duration,
    pub max_concurrent: usize,
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self {
            timeout: LOGIN_TIMEOUT,
            max_concurrent: MAX_CONCURRENT_LOGINS,
        }
    }
}

/// Snapshot of one sign-in, safe to return to a browser: it carries the link
/// and device code the operator needs and no credential material.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LoginSessionView {
    pub session_id: String,
    pub provider_id: String,
    pub phase: LoginPhase,
    /// Human-readable status of the current phase, or why it ended.
    pub detail: String,
    /// Page the operator opens in their browser. Present from `waiting`.
    pub verification_url: Option<String>,
    /// Device code to enter on that page, for device-code providers.
    pub user_code: Option<String>,
    /// True when the operator must paste a code back into Temps.
    pub requires_code: bool,
    pub started_at: DateTime<Utc>,
    pub elapsed_seconds: u64,
    pub timeout_seconds: u64,
    pub finished_at: Option<DateTime<Utc>>,
}

enum SessionCommand {
    SubmitCode {
        code: String,
        reply: oneshot::Sender<Result<(), LoginError>>,
    },
    Cancel,
}

struct SessionState {
    phase: LoginPhase,
    detail: String,
    challenge: Option<LoginChallenge>,
    code_submitted: bool,
    finished_at: Option<DateTime<Utc>>,
    elapsed_at_finish: Option<Duration>,
}

struct Session {
    id: String,
    provider_id: String,
    started_at: DateTime<Utc>,
    started: Instant,
    timeout: Duration,
    state: Mutex<SessionState>,
    phase_tx: watch::Sender<LoginPhase>,
    commands: mpsc::Sender<SessionCommand>,
}

impl Session {
    fn lock(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn set_phase(&self, phase: LoginPhase, detail: impl Into<String>) {
        {
            let mut state = self.lock();
            if state.phase.is_terminal() {
                return;
            }
            state.phase = phase;
            state.detail = detail.into();
            if phase.is_terminal() {
                state.finished_at = Some(Utc::now());
                state.elapsed_at_finish = Some(self.started.elapsed());
            }
        }
        let _ = self.phase_tx.send(phase);
    }

    fn view(&self) -> LoginSessionView {
        let state = self.lock();
        LoginSessionView {
            session_id: self.id.clone(),
            provider_id: self.provider_id.clone(),
            phase: state.phase,
            detail: state.detail.clone(),
            verification_url: state
                .challenge
                .as_ref()
                .map(|challenge| challenge.verification_url.clone()),
            user_code: state
                .challenge
                .as_ref()
                .and_then(|challenge| challenge.user_code.clone()),
            requires_code: state
                .challenge
                .as_ref()
                .map(|challenge| challenge.requires_code)
                .unwrap_or(false),
            started_at: self.started_at,
            elapsed_seconds: state
                .elapsed_at_finish
                .unwrap_or_else(|| self.started.elapsed())
                .as_secs(),
            timeout_seconds: self.timeout.as_secs(),
            finished_at: state.finished_at,
        }
    }

    fn is_terminal(&self) -> bool {
        self.lock().phase.is_terminal()
    }
}

pub struct ProviderLoginService {
    state_root: PathBuf,
    identity: HostIdentity,
    limits: LoginLimits,
    resolver: LoginProviderResolver,
    sink: Arc<dyn LoginCredentialSink>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl ProviderLoginService {
    /// `state_root` is the directory that holds Temps-owned CLI state; it must
    /// be the same root registered with [`host_cli::set_state_root`] so status
    /// and turns read what a sign-in produced.
    pub fn new(state_root: PathBuf, sink: Arc<dyn LoginCredentialSink>) -> Self {
        Self::with_parts(
            state_root,
            HostIdentity::resolve(),
            LoginLimits::default(),
            Arc::new(crate::ai_cli::create_provider),
            sink,
        )
    }

    pub fn with_parts(
        state_root: PathBuf,
        identity: HostIdentity,
        limits: LoginLimits,
        resolver: LoginProviderResolver,
        sink: Arc<dyn LoginCredentialSink>,
    ) -> Self {
        Self {
            state_root,
            identity,
            limits,
            resolver,
            sink,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Begin signing `provider_id` in. Returns immediately in the `starting`
    /// phase; poll [`Self::status`] for the link.
    pub async fn start(
        &self,
        provider_id: &str,
        actor: LoginActor,
    ) -> Result<LoginSessionView, LoginError> {
        let provider = (self.resolver)(provider_id).ok_or_else(|| LoginError::UnknownProvider {
            provider_id: provider_id.to_string(),
        })?;
        if !provider.supports_login() {
            return Err(LoginError::NotSupported {
                provider_id: provider_id.to_string(),
            });
        }

        let id = random_hex(8);
        let scratch_home = self
            .state_root
            .join(provider_id)
            .join(format!("login-{id}"));
        let (commands_tx, commands_rx) = mpsc::channel(8);
        let (phase_tx, _phase_rx) = watch::channel(LoginPhase::Starting);
        let session = Arc::new(Session {
            id: id.clone(),
            provider_id: provider_id.to_string(),
            started_at: Utc::now(),
            started: Instant::now(),
            timeout: self.limits.timeout,
            state: Mutex::new(SessionState {
                phase: LoginPhase::Starting,
                detail: "Starting the sign-in".to_string(),
                challenge: None,
                code_submitted: false,
                finished_at: None,
                elapsed_at_finish: None,
            }),
            phase_tx,
            commands: commands_tx,
        });

        {
            let mut sessions = self.sessions();
            if let Some(existing) = sessions.get(provider_id) {
                if !existing.is_terminal() {
                    return Err(LoginError::AlreadyInProgress {
                        provider_id: provider_id.to_string(),
                    });
                }
            }
            let active = sessions
                .values()
                .filter(|session| !session.is_terminal())
                .count();
            if active >= self.limits.max_concurrent {
                return Err(LoginError::ConcurrencyLimit {
                    limit: self.limits.max_concurrent,
                });
            }
            sessions.insert(provider_id.to_string(), session.clone());
        }

        let view = session.view();
        let driver = Driver {
            session,
            provider,
            sink: self.sink.clone(),
            actor,
            identity: self.identity.clone(),
            state_root: self.state_root.clone(),
            scratch_home,
            timeout: self.limits.timeout,
        };
        tokio::spawn(driver.run(commands_rx));
        Ok(view)
    }

    /// Current sign-in for `provider_id`, if one was started since boot.
    pub fn status(&self, provider_id: &str) -> Option<LoginSessionView> {
        self.sessions()
            .get(provider_id)
            .map(|session| session.view())
    }

    /// Hand the operator's pasted code to the CLI.
    pub async fn submit_code(
        &self,
        provider_id: &str,
        code: &str,
    ) -> Result<LoginSessionView, LoginError> {
        let code = validate_pasted_code(code)?;
        let session =
            self.sessions()
                .get(provider_id)
                .cloned()
                .ok_or_else(|| LoginError::NoSession {
                    provider_id: provider_id.to_string(),
                })?;
        {
            let state = session.lock();
            let waiting_for_code = state.phase == LoginPhase::Waiting
                && !state.code_submitted
                && state
                    .challenge
                    .as_ref()
                    .map(|challenge| challenge.requires_code)
                    .unwrap_or(false);
            if !waiting_for_code {
                return Err(LoginError::NotWaitingForCode {
                    provider_id: provider_id.to_string(),
                    phase: state.phase.as_str().to_string(),
                });
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        session
            .commands
            .send(SessionCommand::SubmitCode {
                code,
                reply: reply_tx,
            })
            .await
            .map_err(|_| LoginError::NotWaitingForCode {
                provider_id: provider_id.to_string(),
                phase: session.lock().phase.as_str().to_string(),
            })?;
        reply_rx.await.map_err(|_| LoginError::NotWaitingForCode {
            provider_id: provider_id.to_string(),
            phase: session.lock().phase.as_str().to_string(),
        })??;
        Ok(session.view())
    }

    /// Stop the sign-in and wait (briefly) for its process to be gone.
    /// Idempotent: cancelling a finished sign-in returns its final state.
    pub async fn cancel(&self, provider_id: &str) -> Result<LoginSessionView, LoginError> {
        let session =
            self.sessions()
                .get(provider_id)
                .cloned()
                .ok_or_else(|| LoginError::NoSession {
                    provider_id: provider_id.to_string(),
                })?;
        if session.is_terminal() {
            return Ok(session.view());
        }
        let _ = session.commands.send(SessionCommand::Cancel).await;
        let mut phase = session.phase_tx.subscribe();
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            phase.wait_for(|phase| phase.is_terminal()),
        )
        .await;
        Ok(session.view())
    }
}

fn random_hex(bytes: usize) -> String {
    use rand::Rng;
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    hex::encode(buffer)
}

/// Background task state for one sign-in.
struct Driver {
    session: Arc<Session>,
    provider: Box<dyn AiCliProvider>,
    sink: Arc<dyn LoginCredentialSink>,
    actor: LoginActor,
    identity: HostIdentity,
    state_root: PathBuf,
    scratch_home: PathBuf,
    timeout: Duration,
}

impl Driver {
    async fn run(self, mut commands: mpsc::Receiver<SessionCommand>) {
        let provider_id = self.session.provider_id.clone();
        let outcome = self.drive(&mut commands).await;
        match outcome {
            Ok(detail) => self.session.set_phase(LoginPhase::Succeeded, detail),
            Err(LoginError::Cancelled { .. }) => {
                self.session
                    .set_phase(LoginPhase::Cancelled, "The sign-in was cancelled");
            }
            Err(error) => {
                tracing::warn!(
                    provider_id = %provider_id,
                    session_id = %self.session.id,
                    error = %error,
                    "AI provider sign-in failed"
                );
                self.session
                    .set_phase(LoginPhase::Failed, error.to_string());
            }
        }
        self.discard_scratch().await;
        // A refreshed host status must be visible to the very next catalog read.
        crate::ai_cli::invalidate_status_cache().await;
    }

    async fn discard_scratch(&self) {
        if let Err(error) = tokio::fs::remove_dir_all(&self.scratch_home).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    provider_id = %self.session.provider_id,
                    path = %self.scratch_home.display(),
                    error = %error,
                    "could not remove sign-in scratch directory"
                );
            }
        }
    }

    fn scratch_location(&self) -> Result<ProviderLocation, LoginError> {
        ProviderLocation::below_home(&self.session.provider_id, &self.scratch_home).ok_or_else(
            || LoginError::NotSupported {
                provider_id: self.session.provider_id.clone(),
            },
        )
    }

    async fn drive(
        &self,
        commands: &mut mpsc::Receiver<SessionCommand>,
    ) -> Result<String, LoginError> {
        let provider_id = self.session.provider_id.clone();
        let deadline = tokio::time::Instant::now() + self.timeout;
        let timed_out = || LoginError::Timeout {
            provider_id: provider_id.clone(),
            timeout_secs: self.timeout.as_secs(),
        };
        let io_error = |operation: &str, error: std::io::Error| LoginError::Io {
            provider_id: provider_id.clone(),
            operation: operation.to_string(),
            reason: error.to_string(),
        };

        let scratch = self.scratch_location()?;
        // Every directory on the path must be reachable by the CLI's user.
        for dir in [
            self.state_root.as_path(),
            self.state_root.join(&provider_id).as_path(),
        ] {
            host_cli::prepare_private_dir(dir, &self.identity)
                .map_err(|error| io_error("preparing the CLI state directory", error))?;
        }

        // Start the CLI, staying responsive to cancellation while it spins up.
        let begin = self.provider.begin_login(&scratch.config_dir);
        tokio::pin!(begin);
        let handle = loop {
            tokio::select! {
                result = &mut begin => break result?,
                command = commands.recv() => match command {
                    Some(SessionCommand::SubmitCode { reply, .. }) => {
                        let _ = reply.send(Err(LoginError::NotWaitingForCode {
                            provider_id: provider_id.clone(),
                            phase: LoginPhase::Starting.as_str().to_string(),
                        }));
                    }
                    Some(SessionCommand::Cancel) | None => {
                        return Err(LoginError::Cancelled { provider_id: provider_id.clone() });
                    }
                },
                _ = tokio::time::sleep_until(deadline) => return Err(timed_out()),
            }
        };

        let detail = if handle.challenge.requires_code {
            "Open the link, approve access, then paste the code shown on the page here."
        } else {
            "Open the link and enter the code shown below. This page continues automatically."
        };
        {
            let mut state = self.session.lock();
            state.challenge = Some(handle.challenge.clone());
        }
        self.session.set_phase(LoginPhase::Waiting, detail);

        let control = handle.control.clone();
        let completion = handle.completion;
        tokio::pin!(completion);
        loop {
            tokio::select! {
                result = &mut completion => {
                    result?;
                    break;
                }
                command = commands.recv() => match command {
                    Some(SessionCommand::SubmitCode { code, reply }) => {
                        match control.submit_code(&code).await {
                            Ok(()) => {
                                self.session.lock().code_submitted = true;
                                self.session.set_phase(
                                    LoginPhase::Verifying,
                                    "Code submitted. Waiting for the CLI to confirm the sign-in.",
                                );
                                let _ = reply.send(Ok(()));
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                    Some(SessionCommand::Cancel) | None => {
                        control.cancel();
                        // The completion future kills the process group and
                        // reaps it; bound the wait so a stuck reap cannot hang.
                        let _ = tokio::time::timeout(Duration::from_secs(5), &mut completion).await;
                        return Err(LoginError::Cancelled { provider_id: provider_id.clone() });
                    }
                },
                _ = tokio::time::sleep_until(deadline) => {
                    control.cancel();
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut completion).await;
                    return Err(timed_out());
                }
            }
        }

        self.session.set_phase(
            LoginPhase::Verifying,
            "The CLI finished. Confirming the sign-in.",
        );
        let finish = self.finalize(&scratch);
        match tokio::time::timeout_at(deadline, finish).await {
            Ok(result) => result,
            Err(_) => Err(timed_out()),
        }
    }

    /// Verify, snapshot, save and promote a finished sign-in.
    async fn finalize(&self, scratch: &ProviderLocation) -> Result<String, LoginError> {
        let provider_id = self.session.provider_id.as_str();

        let status = self.provider.verify_login(&scratch.config_dir).await?;
        if !(status.installed && status.authenticated) {
            return Err(LoginError::NotAuthenticated {
                provider_id: provider_id.to_string(),
                installed: status.installed,
            });
        }

        let credential = read_snapshot(provider_id, &scratch.credential_file).await?;
        let auth_type = "subscription";
        self.sink
            .store(StoreLoginCredential {
                provider_id,
                auth_type,
                credential: &credential,
                actor: &self.actor,
            })
            .await?;

        let canonical =
            ProviderLocation::below_home(provider_id, &self.state_root.join(provider_id))
                .ok_or_else(|| LoginError::NotSupported {
                    provider_id: provider_id.to_string(),
                })?;
        promote(scratch, &canonical, &self.identity).map_err(|error| LoginError::Io {
            provider_id: provider_id.to_string(),
            operation: "activating the new sign-in".to_string(),
            reason: error.to_string(),
        })?;

        Ok(match status.subscription_type.as_deref() {
            Some(plan) if !plan.is_empty() => format!("Signed in ({plan} plan)."),
            _ => "Signed in.".to_string(),
        })
    }
}

async fn read_snapshot(provider_id: &str, file: &Path) -> Result<String, LoginError> {
    let result = match provider_id {
        "claude_cli" => read_claude_snapshot(file)
            .await
            .map(|snapshot| snapshot.to_stored_json()),
        "codex_cli" => read_codex_snapshot(file).await,
        other => {
            return Err(LoginError::NotSupported {
                provider_id: other.to_string(),
            })
        }
    };
    result.map_err(|error| match error {
        CredentialSnapshotError::Io { ref reason, .. } if !file.exists() => {
            tracing::debug!(provider_id, %reason, "credential file missing after sign-in");
            LoginError::CredentialFileMissing {
                provider_id: provider_id.to_string(),
            }
        }
        other => LoginError::CredentialImport {
            provider_id: provider_id.to_string(),
            reason: other.to_string(),
        },
    })
}

/// Replace the live CLI directory with the scratch one. The previous
/// directory is moved aside first and restored if the swap fails, so a
/// working sign-in is never left half-replaced.
fn promote(
    scratch: &ProviderLocation,
    canonical: &ProviderLocation,
    identity: &HostIdentity,
) -> std::io::Result<()> {
    host_cli::prepare_private_dir(&canonical.home, identity)?;
    if !canonical.config_dir.exists() {
        return std::fs::rename(&scratch.config_dir, &canonical.config_dir);
    }
    let aside = canonical.home.join(format!(".replaced-{}", random_hex(4)));
    std::fs::rename(&canonical.config_dir, &aside)?;
    if let Err(error) = std::fs::rename(&scratch.config_dir, &canonical.config_dir) {
        let _ = std::fs::rename(&aside, &canonical.config_dir);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(&aside);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_cli::claude::{begin_claude_login, ClaudeLoginOptions};
    use crate::ai_cli::codex::{begin_codex_login, CodexLoginOptions};
    use crate::ai_cli::login::LoginHandle;
    use crate::ai_cli::{AiCliStatus, AiRunConfig, AiRunResult};
    use crate::error::AgentError;
    use std::os::unix::fs::PermissionsExt;

    /// A provider whose CLI is a shell script, driven through the real Claude
    /// login driver so process handling, link scraping and kill-on-cancel are
    /// the production code paths.
    struct ScriptedProvider {
        id: &'static str,
        program: PathBuf,
        challenge_timeout: Duration,
    }

    #[async_trait]
    impl AiCliProvider for ScriptedProvider {
        fn name(&self) -> &str {
            self.id
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> AiCliStatus {
            unreachable!("sign-in verification uses verify_login")
        }
        fn supports_login(&self) -> bool {
            true
        }
        async fn begin_login(&self, config_dir: &Path) -> Result<LoginHandle, LoginError> {
            let location =
                ProviderLocation::from_config_dir(self.id, config_dir).ok_or_else(|| {
                    LoginError::NotSupported {
                        provider_id: self.id.to_string(),
                    }
                })?;
            if self.id == "codex_cli" {
                return begin_codex_login(CodexLoginOptions {
                    program: self.program.clone().into_os_string(),
                    identity: HostIdentity::current(),
                    location,
                    challenge_timeout: self.challenge_timeout,
                })
                .await;
            }
            begin_claude_login(ClaudeLoginOptions {
                program: self.program.clone().into_os_string(),
                identity: HostIdentity::current(),
                location,
                challenge_timeout: self.challenge_timeout,
            })
            .await
        }
        async fn verify_login(&self, config_dir: &Path) -> Result<AiCliStatus, LoginError> {
            let file = if self.id == "codex_cli" {
                "auth.json"
            } else {
                ".credentials.json"
            };
            let authenticated = config_dir.join(file).is_file();
            Ok(AiCliStatus {
                provider: self.id.to_string(),
                installed: true,
                version: None,
                authenticated,
                auth_method: Some("claude.ai".to_string()),
                email: None,
                subscription_type: Some("max".to_string()),
                setup_hint: None,
            })
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            unreachable!("not used by sign-in tests")
        }
        async fn continue_conversation(
            &self,
            _config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            unreachable!("not used by sign-in tests")
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        stored: Mutex<Vec<(String, String)>>,
        fail: bool,
    }

    #[async_trait]
    impl LoginCredentialSink for RecordingSink {
        async fn store(&self, request: StoreLoginCredential<'_>) -> Result<(), LoginError> {
            if self.fail {
                return Err(LoginError::CredentialImport {
                    provider_id: request.provider_id.to_string(),
                    reason: "settings write refused".to_string(),
                });
            }
            self.stored
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((
                    request.provider_id.to_string(),
                    request.credential.to_string(),
                ));
            Ok(())
        }
    }

    const GOOD_LINK: &str =
        "If the browser didn't open, visit: https://claude.ai/oauth/authorize?code=true&state=s1";

    fn credential_script(link_line: &str, after_link: &str) -> String {
        format!(
            r#"#!/bin/sh
echo "Opening browser to sign in..."
echo "{link_line}"
{after_link}
"#
        )
    }

    /// Reads one line, accepts only `good-code`, then writes a credential file.
    const ACCEPT_GOOD_CODE: &str = r#"printf "Paste code here if prompted > "
read code
if [ "$code" = "good-code" ]; then
  mkdir -p "$CLAUDE_CONFIG_DIR"
  cat > "$CLAUDE_CONFIG_DIR/.credentials.json" <<'EOF'
{"claudeAiOauth":{"accessToken":"at-123","refreshToken":"rt-456","expiresAt":4102444800000,"scopes":["user:inference"],"subscriptionType":"max"}}
EOF
  exit 0
fi
exit 1"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        sink: Arc<RecordingSink>,
    }

    fn fixture_with_sink(sink: RecordingSink) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("state");
        Fixture {
            root,
            _dir: dir,
            sink: Arc::new(sink),
        }
    }

    fn write_script(fixture: &Fixture, name: &str, body: &str) -> PathBuf {
        let path = fixture._dir.path().join(name);
        std::fs::write(&path, body).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        path
    }

    fn service(
        fixture: &Fixture,
        program: PathBuf,
        limits: LoginLimits,
        challenge_timeout: Duration,
    ) -> ProviderLoginService {
        let resolver: LoginProviderResolver = Arc::new(move |id| {
            let id: &'static str = match id {
                "claude_cli" => "claude_cli",
                "codex_cli" => "codex_cli",
                _ => return None,
            };
            Some(Box::new(ScriptedProvider {
                id,
                program: program.clone(),
                challenge_timeout,
            }) as Box<dyn AiCliProvider>)
        });
        ProviderLoginService::with_parts(
            fixture.root.clone(),
            HostIdentity::current(),
            limits,
            resolver,
            fixture.sink.clone(),
        )
    }

    fn actor() -> LoginActor {
        LoginActor {
            user_id: 7,
            ip_address: Some("203.0.113.9".to_string()),
            user_agent: "test".to_string(),
        }
    }

    async fn wait_for_phase(
        service: &ProviderLoginService,
        provider_id: &str,
        wanted: LoginPhase,
    ) -> LoginSessionView {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let view = service.status(provider_id).expect("session exists");
            if view.phase == wanted {
                return view;
            }
            assert!(
                !view.phase.is_terminal() || view.phase == wanted,
                "sign-in ended in {:?} ({}) while waiting for {:?}",
                view.phase,
                view.detail,
                wanted
            );
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {wanted:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_terminal(
        service: &ProviderLoginService,
        provider_id: &str,
    ) -> LoginSessionView {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let view = service.status(provider_id).expect("session exists");
            if view.phase.is_terminal() {
                return view;
            }
            assert!(Instant::now() < deadline, "sign-in never finished");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[tokio::test]
    async fn happy_path_imports_an_access_token_only_snapshot_and_promotes_the_login() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-ok",
            &credential_script(GOOD_LINK, ACCEPT_GOOD_CODE),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        let started = service.start("claude_cli", actor()).await.expect("start");
        assert_eq!(started.phase, LoginPhase::Starting);

        let waiting = wait_for_phase(&service, "claude_cli", LoginPhase::Waiting).await;
        assert!(waiting
            .verification_url
            .as_deref()
            .unwrap_or_default()
            .starts_with("https://claude.ai/oauth/authorize"));
        assert!(waiting.requires_code);
        assert!(!waiting.detail.is_empty());

        let verifying = service
            .submit_code("claude_cli", "good-code")
            .await
            .expect("submit");
        assert_eq!(verifying.phase, LoginPhase::Verifying);

        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Succeeded, "{}", done.detail);
        assert!(done.finished_at.is_some());

        let stored = fixture.sink.stored.lock().expect("lock").clone();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].1.contains("at-123"));
        assert!(
            !stored[0].1.contains("rt-456"),
            "the refresh token must never reach the saved snapshot"
        );

        // The CLI-owned store was promoted and the scratch directory removed.
        let live = fixture.root.join("claude_cli/.claude/.credentials.json");
        assert!(live.is_file(), "promoted credential missing");
        let leftovers = std::fs::read_dir(fixture.root.join("claude_cli"))
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("login-"))
            .count();
        assert_eq!(leftovers, 0, "scratch directory not cleaned up");
    }

    const CHALLENGE_SHORT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn wrong_code_fails_without_touching_the_saved_credential() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-bad",
            &credential_script(GOOD_LINK, ACCEPT_GOOD_CODE),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("claude_cli", actor()).await.expect("start");
        wait_for_phase(&service, "claude_cli", LoginPhase::Waiting).await;
        service
            .submit_code("claude_cli", "wrong-code")
            .await
            .expect("submit");

        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("rejected the code"), "{}", done.detail);
        assert!(fixture.sink.stored.lock().expect("lock").is_empty());
        assert!(!fixture.root.join("claude_cli/.claude").exists());
    }

    #[tokio::test]
    async fn times_out_and_kills_the_child() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let pid_file = fixture._dir.path().join("pid");
        let body = credential_script(
            GOOD_LINK,
            &format!("echo $$ > '{}'\nsleep 60", pid_file.display()),
        );
        let script = write_script(&fixture, "claude-hang", &body);
        let service = service(
            &fixture,
            script,
            LoginLimits {
                timeout: Duration::from_millis(1500),
                max_concurrent: 4,
            },
            CHALLENGE_SHORT,
        );

        service.start("claude_cli", actor()).await.expect("start");
        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("timed out"), "{}", done.detail);

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid) {
            assert!(
                Instant::now() < deadline,
                "CLI process {pid} survived the timeout"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn cancel_kills_the_child_and_reports_cancelled() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let pid_file = fixture._dir.path().join("pid");
        let body = credential_script(
            GOOD_LINK,
            &format!("echo $$ > '{}'\nsleep 60", pid_file.display()),
        );
        let script = write_script(&fixture, "claude-cancel", &body);
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("claude_cli", actor()).await.expect("start");
        wait_for_phase(&service, "claude_cli", LoginPhase::Waiting).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.is_file() {
            assert!(Instant::now() < deadline, "script never wrote its pid");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        assert!(process_alive(pid));

        let cancelled = service.cancel("claude_cli").await.expect("cancel");
        assert_eq!(cancelled.phase, LoginPhase::Cancelled);

        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid) {
            assert!(
                Instant::now() < deadline,
                "CLI process {pid} survived cancel"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Cancelling again is a no-op that reports the final state.
        let again = service.cancel("claude_cli").await.expect("cancel again");
        assert_eq!(again.phase, LoginPhase::Cancelled);
    }

    #[tokio::test]
    async fn concurrency_cap_rejects_the_extra_sign_in() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-wait",
            &credential_script(GOOD_LINK, "sleep 60"),
        );
        let service = service(
            &fixture,
            script,
            LoginLimits {
                timeout: Duration::from_secs(60),
                max_concurrent: 1,
            },
            CHALLENGE_SHORT,
        );

        service.start("claude_cli", actor()).await.expect("first");
        let error = service
            .start("codex_cli", actor())
            .await
            .expect_err("second must be refused");
        assert!(
            matches!(error, LoginError::ConcurrencyLimit { limit: 1 }),
            "{error}"
        );

        // Once the first finishes, capacity is available again.
        service.cancel("claude_cli").await.expect("cancel");
        service
            .start("codex_cli", actor())
            .await
            .expect("capacity freed");
        service.cancel("codex_cli").await.expect("cancel second");
    }

    #[tokio::test]
    async fn second_start_for_the_same_provider_is_a_conflict() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-wait2",
            &credential_script(GOOD_LINK, "sleep 60"),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("claude_cli", actor()).await.expect("first");
        let error = service
            .start("claude_cli", actor())
            .await
            .expect_err("duplicate");
        assert!(matches!(error, LoginError::AlreadyInProgress { .. }));
        service.cancel("claude_cli").await.expect("cancel");
    }

    #[tokio::test]
    async fn link_outside_the_allow_list_fails_the_sign_in() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-evil",
            &credential_script(
                "visit https://evil.example/oauth/authorize?state=zz",
                "sleep 60",
            ),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("claude_cli", actor()).await.expect("start");
        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("evil.example"), "{}", done.detail);
        assert!(done.verification_url.is_none());
        assert!(!done.detail.contains("state=zz"));
    }

    #[tokio::test]
    async fn missing_cli_reports_not_installed() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let service = service(
            &fixture,
            fixture._dir.path().join("does-not-exist"),
            LoginLimits::default(),
            CHALLENGE_SHORT,
        );

        service.start("claude_cli", actor()).await.expect("start");
        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("not installed"), "{}", done.detail);
    }

    #[tokio::test]
    async fn unsupported_and_unknown_providers_are_refused() {
        struct NoLogin;
        #[async_trait]
        impl AiCliProvider for NoLogin {
            fn name(&self) -> &str {
                "opencode"
            }
            async fn check_installed(&self) -> bool {
                true
            }
            async fn get_status(&self) -> AiCliStatus {
                unreachable!()
            }
            async fn run(&self, _c: AiRunConfig) -> Result<AiRunResult, AgentError> {
                unreachable!()
            }
            async fn continue_conversation(
                &self,
                _c: AiRunConfig,
            ) -> Result<AiRunResult, AgentError> {
                unreachable!()
            }
        }
        let fixture = fixture_with_sink(RecordingSink::default());
        let resolver: LoginProviderResolver =
            Arc::new(|id| (id == "opencode").then(|| Box::new(NoLogin) as Box<dyn AiCliProvider>));
        let service = ProviderLoginService::with_parts(
            fixture.root.clone(),
            HostIdentity::current(),
            LoginLimits::default(),
            resolver,
            fixture.sink.clone(),
        );
        assert!(matches!(
            service.start("opencode", actor()).await,
            Err(LoginError::NotSupported { .. })
        ));
        assert!(matches!(
            service.start("nope", actor()).await,
            Err(LoginError::UnknownProvider { .. })
        ));
    }

    #[tokio::test]
    async fn codes_are_refused_unless_the_sign_in_is_waiting_for_one() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(
            &fixture,
            "claude-wait3",
            &credential_script(GOOD_LINK, "sleep 60"),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        assert!(matches!(
            service.submit_code("claude_cli", "abc").await,
            Err(LoginError::NoSession { .. })
        ));
        service.start("claude_cli", actor()).await.expect("start");
        wait_for_phase(&service, "claude_cli", LoginPhase::Waiting).await;
        assert!(matches!(
            service.submit_code("claude_cli", "has space").await,
            Err(LoginError::InvalidCode { .. })
        ));
        assert!(matches!(
            service.submit_code("claude_cli", "").await,
            Err(LoginError::InvalidCode { .. })
        ));
        service.cancel("claude_cli").await.expect("cancel");
        assert!(matches!(
            service.submit_code("claude_cli", "abc").await,
            Err(LoginError::NotWaitingForCode { .. })
        ));
    }

    #[tokio::test]
    async fn a_sink_failure_fails_the_sign_in_and_keeps_the_previous_login() {
        let fixture = fixture_with_sink(RecordingSink {
            fail: true,
            ..Default::default()
        });
        // A previously working sign-in must survive a failed replacement.
        let live = fixture.root.join("claude_cli/.claude");
        std::fs::create_dir_all(&live).expect("live dir");
        std::fs::write(live.join(".credentials.json"), "previous").expect("previous");
        let script = write_script(
            &fixture,
            "claude-sinkfail",
            &credential_script(GOOD_LINK, ACCEPT_GOOD_CODE),
        );
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("claude_cli", actor()).await.expect("start");
        wait_for_phase(&service, "claude_cli", LoginPhase::Waiting).await;
        service
            .submit_code("claude_cli", "good-code")
            .await
            .expect("submit");
        let done = wait_for_terminal(&service, "claude_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert_eq!(
            std::fs::read_to_string(live.join(".credentials.json")).expect("read"),
            "previous"
        );
    }

    const CODEX_DEVICE_SCRIPT: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *account/login/start*)
      echo '{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"L1","verificationUrl":"https://auth.openai.com/codex/device","userCode":"ABCD-1234"}}'
      sleep 1
      mkdir -p "$CODEX_HOME"
      cat > "$CODEX_HOME/auth.json" <<'EOF'
{"tokens":{"id_token":"id","access_token":"codex-access","refresh_token":"codex-rotating","account_id":"acct"}}
EOF
      echo '{"method":"account/login/completed","params":{"loginId":"L1","success":true}}'
      sleep 30
      ;;
  esac
done
"#;

    #[tokio::test]
    async fn codex_device_code_flow_completes_without_a_pasted_code() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let script = write_script(&fixture, "codex-ok", CODEX_DEVICE_SCRIPT);
        let service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);

        service.start("codex_cli", actor()).await.expect("start");
        let waiting = wait_for_phase(&service, "codex_cli", LoginPhase::Waiting).await;
        assert_eq!(waiting.user_code.as_deref(), Some("ABCD-1234"));
        assert!(!waiting.requires_code);
        assert_eq!(
            waiting.verification_url.as_deref(),
            Some("https://auth.openai.com/codex/device")
        );
        // Device-code flows take no pasted code.
        assert!(matches!(
            service.submit_code("codex_cli", "abc").await,
            Err(LoginError::NotWaitingForCode { .. })
        ));

        let done = wait_for_terminal(&service, "codex_cli").await;
        assert_eq!(done.phase, LoginPhase::Succeeded, "{}", done.detail);
        let stored = fixture.sink.stored.lock().expect("lock").clone();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].1.contains("codex-access"));
        assert!(
            !stored[0].1.contains("codex-rotating"),
            "the refresh token must never reach the saved snapshot"
        );
        assert!(fixture.root.join("codex_cli/.codex/auth.json").is_file());
    }

    #[tokio::test]
    async fn codex_declined_sign_in_fails_and_off_list_hosts_are_refused() {
        let fixture = fixture_with_sink(RecordingSink::default());
        let declined = CODEX_DEVICE_SCRIPT.replace("\"success\":true", "\"success\":false");
        let script = write_script(&fixture, "codex-declined", &declined);
        let declined_service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);
        declined_service
            .start("codex_cli", actor())
            .await
            .expect("start");
        let done = wait_for_terminal(&declined_service, "codex_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("did not complete"), "{}", done.detail);
        assert!(fixture.sink.stored.lock().expect("lock").is_empty());

        let evil = CODEX_DEVICE_SCRIPT.replace("auth.openai.com", "evil.example");
        let script = write_script(&fixture, "codex-evil", &evil);
        let evil_service = service(&fixture, script, LoginLimits::default(), CHALLENGE_SHORT);
        evil_service
            .start("codex_cli", actor())
            .await
            .expect("start");
        let done = wait_for_terminal(&evil_service, "codex_cli").await;
        assert_eq!(done.phase, LoginPhase::Failed);
        assert!(done.detail.contains("evil.example"), "{}", done.detail);
        assert!(done.verification_url.is_none());
    }

    #[test]
    fn promote_swaps_directories_and_removes_the_old_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = ProviderLocation::below_home("claude_cli", &dir.path().join("claude_cli"))
            .expect("canonical");
        let scratch =
            ProviderLocation::below_home("claude_cli", &dir.path().join("claude_cli/login-1"))
                .expect("scratch");
        std::fs::create_dir_all(&canonical.config_dir).expect("old");
        std::fs::write(canonical.config_dir.join("old"), "old").expect("old file");
        std::fs::create_dir_all(&scratch.config_dir).expect("new");
        std::fs::write(scratch.config_dir.join("new"), "new").expect("new file");

        promote(&scratch, &canonical, &HostIdentity::current()).expect("promote");

        assert!(canonical.config_dir.join("new").is_file());
        assert!(!canonical.config_dir.join("old").exists());
        let aside = std::fs::read_dir(&canonical.home)
            .expect("read")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".replaced-"))
            .count();
        assert_eq!(aside, 0);
    }
}
