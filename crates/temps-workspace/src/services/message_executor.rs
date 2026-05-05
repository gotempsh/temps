use sea_orm::{DatabaseConnection, EntityTrait};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use temps_agents::ai_cli::OnEventCallback;
use temps_agents::sandbox::{SANDBOX_CHOWN, SANDBOX_HOME, SANDBOX_WORK_DIR};
use temps_config::ConfigService;
use temps_core::{EncryptionService, WorkflowMemoryProvider};
use temps_deployments::services::deployment_token_service::{
    CreateDeploymentTokenRequest, DeploymentTokenService,
};
use temps_entities::{projects, settings, users};
use temps_git::services::git_provider_manager_trait::GitProviderManagerTrait;
use temps_providers::ExternalServiceManager;

use crate::error::WorkspaceError;
use crate::services::session_manager::WorkspaceSessionManager;
use crate::services::workspace_service::{
    SendMessageRequest, UpdateSessionFields, WorkspaceService,
};

/// Executes chat messages within a workspace session.
///
/// Responsibilities:
/// - On first message: clone the project repo, create sandbox, inject skill
/// - Run the AI CLI with the user's prompt
/// - Stream AI output as assistant messages into workspace_messages
/// - Update the session's token/cost accounting
pub struct MessageExecutor {
    db: Arc<DatabaseConnection>,
    workspace_service: Arc<WorkspaceService>,
    session_manager: Arc<WorkspaceSessionManager>,
    git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    encryption_service: Arc<EncryptionService>,
    deployment_token_service: Arc<DeploymentTokenService>,
    external_service_manager: Arc<ExternalServiceManager>,
    /// Platform-wide settings — used to resolve the public `external_url`
    /// the in-sandbox CLI must dial when calling back to the API. The
    /// container can't reach `127.0.0.1:3000`, so we hand it the same URL
    /// real users hit (e.g. `https://temps.example.com`).
    platform_config_service: Arc<ConfigService>,
    /// Optional memory provider. When set, the executor pre-loads relevant
    /// workflow memory into the prompt before spawning the harness.
    /// Workspace chat sessions don't currently use this (they have no
    /// associated workflow), but the field is here so that future workflow-run
    /// executors can be wired up the same way.
    ///
    /// Typed as `Arc<dyn WorkflowMemoryProvider>` (not the concrete
    /// `WorkflowMemoryService`) so this consumer stays on the abstract
    /// boundary — any provider passing the `temps-memory` eval harness
    /// can be swapped in (in-memory for tests, DB-backed in prod, a
    /// remote-cache shim tomorrow). See PR 3.2.
    memory_provider: Option<Arc<dyn WorkflowMemoryProvider>>,
    /// Per-session execution locks. Ensures only one Claude CLI run is in
    /// flight per session at a time — concurrent `--continue` invocations
    /// race on the on-disk session state file and silently hang. Holding a
    /// per-session async Mutex serializes them so the second user message
    /// waits politely for the first to finish.
    session_locks: Arc<RwLock<HashMap<i32, Arc<Mutex<()>>>>>,
    /// Cancellation tokens for in-flight runs. Populated at the start of
    /// `execute_message`, removed at the end. The `cancel_run` handler
    /// fires the token to tell the exec loop to bail out early.
    active_runs: Arc<RwLock<HashMap<i32, CancellationToken>>>,
    /// Sessions whose claude jsonl may be in a dirty state (prior run was
    /// cancelled or timed out mid-turn). On the next message, we run a
    /// repair step before invoking `claude --continue`. Cleared on success.
    dirty_sessions: Arc<RwLock<HashSet<i32>>>,
    /// Sessions that currently have a drain loop running. Used to deduplicate
    /// `enqueue_run` calls — the second send_message just queues the message
    /// (already persisted by the handler) and returns; the running loop picks
    /// it up on its next iteration. Distinct from `active_runs` which holds
    /// the per-turn cancellation token.
    draining_sessions: Arc<RwLock<HashSet<i32>>>,
    /// Sessions whose drain loop should bail out at the next turn boundary.
    /// Set by `cancel`. The loop clears it when it exits.
    drain_cancel: Arc<RwLock<HashSet<i32>>>,
    /// Optional: used to resolve + inject secrets, skills, and MCP servers
    /// into workspace sandboxes at session start. When `None`, workspace
    /// sessions skip the injection phase (agents plugin not loaded).
    secret_service: Option<Arc<temps_agents::services::secret_service::SecretService>>,
    definition_service: Option<Arc<temps_agents::services::definition_service::DefinitionService>>,
}

impl MessageExecutor {
    // Constructor takes 8 service Arcs — refactoring into a builder for the
    // sake of one clippy lint is more ceremony than payoff.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DatabaseConnection>,
        workspace_service: Arc<WorkspaceService>,
        session_manager: Arc<WorkspaceSessionManager>,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
        encryption_service: Arc<EncryptionService>,
        deployment_token_service: Arc<DeploymentTokenService>,
        external_service_manager: Arc<ExternalServiceManager>,
        platform_config_service: Arc<ConfigService>,
    ) -> Self {
        Self {
            db,
            workspace_service,
            session_manager,
            git_provider_manager,
            encryption_service,
            deployment_token_service,
            external_service_manager,
            platform_config_service,
            memory_provider: None,
            session_locks: Arc::new(RwLock::new(HashMap::new())),
            active_runs: Arc::new(RwLock::new(HashMap::new())),
            dirty_sessions: Arc::new(RwLock::new(HashSet::new())),
            draining_sessions: Arc::new(RwLock::new(HashSet::new())),
            drain_cancel: Arc::new(RwLock::new(HashSet::new())),
            secret_service: None,
            definition_service: None,
        }
    }

    /// Wire in the agents-plugin services so workspace sessions get the same
    /// skills / MCP / secret injection as agent runs. Call this at plugin
    /// registration time when both services are available.
    pub fn with_injection_services(
        mut self,
        secret_service: Arc<temps_agents::services::secret_service::SecretService>,
        definition_service: Arc<temps_agents::services::definition_service::DefinitionService>,
    ) -> Self {
        self.secret_service = Some(secret_service);
        self.definition_service = Some(definition_service);
        self
    }

    /// Cancel an in-flight run for this session. Called from the cancel
    /// handler. Fires the cancellation token (exec loop bails out on its
    /// next poll) and kicks off a best-effort process-tree kill in the
    /// sandbox. Also marks the session dirty so the next run repairs the
    /// jsonl before invoking --continue.
    pub async fn cancel(&self, session_id: i32) {
        // Tell the drain loop to bail out at the next turn boundary, so any
        // queued user messages are NOT processed. Cancel = stop everything.
        self.drain_cancel.write().await.insert(session_id);
        if let Some(token) = self.active_runs.read().await.get(&session_id).cloned() {
            token.cancel();
        }
        // Mark dirty so next message runs the jsonl repair step.
        self.dirty_sessions.write().await.insert(session_id);
        // SIGTERM first — give claude ~2s to flush the current turn to jsonl.
        self.session_manager
            .kill_session_processes(
                session_id,
                "^claude ",
                temps_agents::sandbox::KillSignal::Term,
            )
            .await;
        // Spawn the escalation-to-SIGKILL so we don't block the handler.
        let sm = self.session_manager.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            sm.kill_session_processes(
                session_id,
                "^claude ",
                temps_agents::sandbox::KillSignal::Kill,
            )
            .await;
        });
    }

    /// Mark a session's claude jsonl as dirty (pending repair on next run).
    /// Used by startup reconciliation for orphaned runs.
    pub async fn mark_dirty(&self, session_id: i32) {
        self.dirty_sessions.write().await.insert(session_id);
    }

    /// Find the most-recently-modified Claude CLI session jsonl inside the
    /// sandbox and repair it if needed. Claude stores session state at
    /// `~/.claude/projects/<encoded-workdir>/<session-id>.jsonl` — we list
    /// the project dir, pick the newest file, and run `repair_claude_jsonl`
    /// on its bytes. Writes back only if the repair changed anything.
    async fn repair_session_jsonl(&self, session_id: i32) -> Result<(), WorkspaceError> {
        // Claude encodes the workdir as a filename by replacing '/' with '-'.
        // The leading slash on the workdir produces a leading dash in the
        // encoded name (e.g. `/home/temps/workspace` → `-home-temps-workspace`).
        let claude_projects_dir = format!(
            "{}/.claude/projects/{}",
            SANDBOX_HOME,
            SANDBOX_WORK_DIR.replace('/', "-")
        );

        // List the directory and find the newest .jsonl. We run `ls -t` via
        // exec — small, bounded, no phantom hang risk.
        let list_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "ls -t {}/*.jsonl 2>/dev/null | head -1",
                claude_projects_dir
            ),
        ];
        let listing = self
            .session_manager
            .exec(session_id, list_cmd, HashMap::new(), None)
            .await?;
        let jsonl_path = listing.stdout.trim().to_string();
        if jsonl_path.is_empty() {
            // No session file yet — nothing to repair. This is normal for
            // a fresh session that was marked dirty before its first run.
            tracing::debug!(
                "repair_session_jsonl: no claude jsonl found for session {}",
                session_id
            );
            return Ok(());
        }

        let raw = self
            .session_manager
            .read_file(session_id, &jsonl_path)
            .await?;

        let (repaired, changed) = repair_claude_jsonl(&raw);
        if !changed {
            tracing::debug!(
                "repair_session_jsonl: session {} jsonl already clean",
                session_id
            );
            return Ok(());
        }

        self.session_manager
            .write_file(session_id, &jsonl_path, &repaired, 0o644)
            .await?;

        tracing::info!(
            "repair_session_jsonl: repaired {} for session {} ({} -> {} bytes)",
            jsonl_path,
            session_id,
            raw.len(),
            repaired.len()
        );
        Ok(())
    }

    /// Read the current git branch from inside the sandbox's `/workspace`.
    /// Returns `Ok(None)` if the dir is not a git repo or HEAD is detached.
    /// Best-effort: errors are logged and converted to `Ok(None)` so callers
    /// can use `.unwrap_or(None)` semantics without aborting their flow.
    pub async fn read_current_branch(&self, session_id: i32) -> Option<String> {
        if !self.session_manager.is_alive(session_id).await {
            return None;
        }
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "git -C {} rev-parse --abbrev-ref HEAD 2>/dev/null",
                SANDBOX_WORK_DIR
            ),
        ];
        match self
            .session_manager
            .exec(session_id, cmd, HashMap::new(), None)
            .await
        {
            Ok(r) if r.exit_code == 0 => {
                let branch = r.stdout.trim().to_string();
                if branch.is_empty() || branch == "HEAD" {
                    None
                } else {
                    Some(branch)
                }
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(
                    "read_current_branch: exec failed for session {}: {}",
                    session_id,
                    e
                );
                None
            }
        }
    }

    /// Sync the cached `branch_name` on the session row to whatever
    /// `/workspace` HEAD currently points at. No-op if unchanged or unreadable.
    async fn sync_current_branch(&self, session_id: i32, cached: Option<&str>) {
        let current = self.read_current_branch(session_id).await;
        if current.as_deref() == cached {
            return;
        }
        if let Some(branch) = current {
            let _ = self
                .workspace_service
                .update_session(
                    session_id,
                    UpdateSessionFields {
                        branch_name: Some(branch),
                        ..Default::default()
                    },
                )
                .await;
        }
    }

    /// Get-or-create the per-session execution lock.
    async fn lock_for(&self, session_id: i32) -> Arc<Mutex<()>> {
        if let Some(lock) = self.session_locks.read().await.get(&session_id) {
            return lock.clone();
        }
        let mut w = self.session_locks.write().await;
        w.entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Attach a workflow memory provider so future runs can pre-load relevant
    /// memory into the prompt before spawning the AI harness.
    ///
    /// Typed as `Arc<dyn WorkflowMemoryProvider>` rather than the concrete
    /// service — see field doc for rationale.
    pub fn with_memory_provider(mut self, memory: Arc<dyn WorkflowMemoryProvider>) -> Self {
        self.memory_provider = Some(memory);
        self
    }

    /// Build the full prompt for a chat turn, including any pre-loaded
    /// workflow memory. For workspace chat sessions (no agent_id), memory
    /// rendering is a no-op and the user message is returned as-is.
    pub(crate) async fn build_chat_prompt(
        &self,
        user_content: &str,
        is_first_message: bool,
        workflow_agent_id: Option<i32>,
        project_id: i32,
        relevant_tags: Vec<String>,
    ) -> String {
        build_chat_prompt_with_memory(
            self.memory_provider.as_deref(),
            user_content,
            is_first_message,
            workflow_agent_id,
            project_id,
            relevant_tags,
        )
        .await
    }
}

/// Default per-trigger memory budget — matches the concrete service's cap.
/// Small enough to fit comfortably in prompts; large enough to surface the
/// most-relevant handful of facts.
const PROMPT_MEMORY_LOAD_LIMIT: usize = 10;

/// Free-function variant of `build_chat_prompt` that takes a memory
/// provider trait object as a parameter. Easier to unit-test in isolation —
/// and by consuming the trait (not the concrete service) the tests can
/// run against any `WorkflowMemoryProvider` impl (in-memory for unit
/// tests, DB-backed in integration).
pub(crate) async fn build_chat_prompt_with_memory(
    memory_provider: Option<&dyn WorkflowMemoryProvider>,
    user_content: &str,
    is_first_message: bool,
    workflow_agent_id: Option<i32>,
    project_id: i32,
    relevant_tags: Vec<String>,
) -> String {
    // Memory is only injected on the first message of a session.
    // Subsequent messages use --continue and inherit the prior context.
    if !is_first_message {
        return user_content.to_string();
    }

    let memory_section = match (memory_provider, workflow_agent_id) {
        (Some(provider), Some(agent_id)) => {
            match provider
                .load_for_trigger(
                    project_id,
                    agent_id,
                    relevant_tags,
                    PROMPT_MEMORY_LOAD_LIMIT,
                )
                .await
            {
                Ok(facts) => provider.render_for_prompt(&facts),
                Err(e) => {
                    tracing::warn!(
                        "Failed to load memory for prompt (agent={}): {}. Continuing without memory.",
                        agent_id,
                        e
                    );
                    String::new()
                }
            }
        }
        _ => String::new(),
    };

    if memory_section.is_empty() {
        user_content.to_string()
    } else {
        format!("{}\n## Current request\n{}", memory_section, user_content)
    }
}

impl MessageExecutor {
    /// Execute a user message end-to-end:
    /// 1. If sandbox not created yet → clone repo + create sandbox + inject skill
    /// 2. Run the AI CLI via session_manager
    /// 3. Stream output as assistant messages
    /// 4. Update token counts
    ///
    /// Drain all pending user messages for a session, one turn at a time.
    ///
    /// Called by the send_message handler after persisting a new user message.
    /// If a drain loop is already running for this session, this is a no-op
    /// (the running loop will pick up the new message on its next iteration).
    /// Otherwise, this spawns a loop that:
    ///   1. Reads all unprocessed user messages on the session
    ///   2. Concatenates them into a single prompt
    ///   3. Runs the AI CLI for one turn
    ///   4. Repeats until no more user messages are pending
    ///
    /// This is the queue-and-drain pattern used by every Claude wrapper —
    /// the input stays enabled while the assistant is thinking, queued
    /// messages stack on the session, and they get merged into the next turn.
    pub async fn enqueue_run(self: &Arc<Self>, session_id: i32) -> Result<(), WorkspaceError> {
        // Atomically check whether a drain loop is already running. If so,
        // the running loop will see the new message in its next DB query —
        // we have nothing to do. Otherwise install the sentinel and spawn
        // the loop. The sentinel is removed when the loop exits.
        {
            let mut draining = self.draining_sessions.write().await;
            if draining.contains(&session_id) {
                tracing::debug!(
                    "Drain loop already running for session {} — message queued",
                    session_id
                );
                return Ok(());
            }
            draining.insert(session_id);
        }
        // Clear any prior drain-cancel flag from a previous run.
        self.drain_cancel.write().await.remove(&session_id);

        let executor = self.clone();
        tokio::spawn(async move {
            if let Err(e) = executor.drain_loop(session_id).await {
                // Swallow errors that are really just user-initiated cancels.
                // The HTTP cancel_run handler already wrote a terminal
                // system+assistant turn for the user, so writing another pair
                // here would produce the duplicate "Run failed: ... cancelled
                // by user" cascade the UI showed.
                let was_cancelled = executor.drain_cancel.read().await.contains(&session_id);
                if was_cancelled {
                    tracing::debug!(
                        "Drain loop for session {} ended via cancel — suppressing error messages",
                        session_id
                    );
                } else {
                    tracing::error!(
                        "Workspace drain loop failed for session {}: {}",
                        session_id,
                        e
                    );
                    let detail = e.to_string();
                    let _ = executor
                        .workspace_service
                        .append_message(SendMessageRequest {
                            session_id,
                            role: "system".to_string(),
                            content: format!("Run failed: {}", detail),
                            metadata: None,
                        })
                        .await;
                    let _ = executor
                        .workspace_service
                        .append_message(SendMessageRequest {
                            session_id,
                            role: "assistant".to_string(),
                            content: format!("Run failed: {}", detail),
                            metadata: Some(serde_json::json!({
                                "error": true,
                                "error_kind": "execution_failed",
                                "detail": detail,
                            })),
                        })
                        .await;
                }
            }
            executor.draining_sessions.write().await.remove(&session_id);
            executor.drain_cancel.write().await.remove(&session_id);
        });

        Ok(())
    }

    /// The actual drain loop. Runs turns until no pending user messages
    /// remain. Each iteration concatenates all currently-pending user
    /// messages into one prompt — fewer turns, lower cost, matches user
    /// mental model of "I'm adding to my thought".
    async fn drain_loop(&self, session_id: i32) -> Result<(), WorkspaceError> {
        // Seed the watermark so we only drain user messages that haven't been
        // answered yet. Without this we'd start from 0 and re-concatenate the
        // entire session transcript into every prompt — the AI CLI already
        // has prior history via --continue, so re-sending it doubles context
        // and produces cumulative answers ("3+3, 2+2, 4+4 = …") when the user
        // only asked for the latest turn.
        let mut last_processed_user_id: i64 = self
            .workspace_service
            .last_answered_user_message_id(session_id)
            .await
            .unwrap_or(0);
        loop {
            // Pull all user messages on this session newer than the last one
            // we processed. Filter out non-user roles to avoid re-running on
            // assistant/system messages we wrote ourselves.
            let pending = self
                .workspace_service
                .get_messages_after(session_id, last_processed_user_id)
                .await?;
            let pending_user: Vec<_> = pending.into_iter().filter(|m| m.role == "user").collect();
            if pending_user.is_empty() {
                return Ok(());
            }
            let max_id = pending_user.last().map(|m| m.id).unwrap_or(0);
            // Concatenate queued user messages with blank-line separators.
            let combined = pending_user
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            // Check for cancellation before each turn. `cancel` sets this
            // flag and the running execute_message will also bail via its
            // own per-turn cancellation token.
            if self.drain_cancel.read().await.contains(&session_id) {
                tracing::debug!("Drain loop cancelled for session {}", session_id);
                return Ok(());
            }

            self.execute_message(session_id, combined).await?;
            last_processed_user_id = max_id;
        }
    }

    /// Refresh a live sandbox in place: re-issues the deployment token,
    /// rewrites `~/.env` (linked services + git tokens + new TEMPS_API_TOKEN),
    /// re-injects the latest `temps-platform.md` skill, and re-installs git
    /// credentials. Does NOT recreate the container — the bind-mounted
    /// work_dir, the home volume, and any in-flight Claude conversation
    /// state are preserved.
    ///
    /// Use this when:
    ///   - The Temps binary has been upgraded and the embedded skill changed
    ///   - The deployment token is about to expire or has been rotated
    ///   - A linked service / git provider token has changed
    ///   - A new email domain was verified and the agent needs to know
    ///
    /// The container's *process env* (`TEMPS_API_TOKEN` set at create time)
    /// stays stale, but the rewritten `~/.env` overrides it whenever the
    /// agent runs `. ~/.env && <cmd>` per the documented procedure in
    /// `~/.claude/CLAUDE.md`. Application code that reads from process env
    /// directly will keep seeing the old token until the sandbox is
    /// recreated — that's a known limitation of in-place refresh.
    pub async fn refresh_sandbox(&self, session_id: i32) -> Result<(), WorkspaceError> {
        // Take the per-session lock so a refresh can't race with an
        // execute_message turn rewriting the same files.
        let lock = self.lock_for(session_id).await;
        let _guard = lock.lock().await;

        let session = self.workspace_service.get_session(session_id).await?;
        if session.status == "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id,
                status: "closed".to_string(),
            });
        }
        if !self.session_manager.is_alive(session_id).await {
            return Err(WorkspaceError::SandboxNotAvailable { session_id });
        }

        let project = projects::Entity::find_by_id(session.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WorkspaceError::ProjectNotFound {
                project_id: session.project_id,
            })?;

        // Re-issue the deployment token. Old token is left to expire on its
        // own — we don't have a revoke path here.
        let session_token = self
            .issue_session_token(session.project_id, session.id)
            .await?;

        // Rebuild the managed env: linked services + git tokens + new
        // TEMPS_API_TOKEN + TEMPS_API_URL. Mirrors the build in
        // initialize_sandbox so the agent's `. ~/.env` view stays consistent.
        let mut managed_env: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        let temps_api_url = self.get_temps_api_url().await;
        managed_env.insert("TEMPS_API_URL".to_string(), temps_api_url.clone());
        managed_env.insert("TEMPS_API_TOKEN".to_string(), session_token.clone());
        managed_env.insert(
            "TEMPS_PROJECT_ID".to_string(),
            session.project_id.to_string(),
        );

        // Persist the token in the in-sandbox CLI's auth files so
        // `bunx @temps-sdk/cli` is authenticated even outside `. ~/.env`.
        if let Err(e) = self
            .write_cli_auth_files(session.id, session.user_id, &temps_api_url, &session_token)
            .await
        {
            tracing::warn!(
                "refresh_sandbox: failed to refresh CLI auth files for session {}: {}",
                session.id,
                e
            );
        }

        // Re-inject credentials for *every* configured provider (not just the
        // active one) so claude/codex/opencode terminal tabs all stay
        // authenticated through a refresh. Catalog dispatch handles the
        // per-flavor split: env-var flavors land in `managed_env` (and thus
        // in `~/.env`); file-based flavors are written directly to disk.
        let provider_env = self.seed_all_configured_providers(session.id).await;
        managed_env.extend(provider_env);

        match self
            .external_service_manager
            .get_project_service_environment_variables(session.project_id)
            .await
        {
            Ok(by_service) => {
                for (_service_id, vars) in by_service {
                    for (k, v) in vars {
                        managed_env.insert(k, v);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "refresh_sandbox: failed to load linked-service env for project {}: {}",
                    session.project_id,
                    e
                );
            }
        }

        // Git tokens deliberately NOT injected — handled by the
        // in-sandbox credential daemon (see temps-git-credential).
        // `git_creds` is preserved as `None` for compatibility with
        // downstream callers; the credential daemon owns this path now.
        let git_creds: Option<(String, String)> = None;

        let project_ctx = crate::services::session_manager::ProjectContext {
            id: project.id,
            slug: &project.slug,
            name: &project.name,
            repo_owner: &project.repo_owner,
            repo_name: &project.repo_name,
            branch: session
                .branch_name
                .as_deref()
                .or(session.base_branch_name.as_deref())
                .unwrap_or(project.main_branch.as_str()),
        };

        // Rewrite ~/.env + ~/.claude/CLAUDE.md, re-inject the platform skill,
        // re-install git credentials. All best-effort with logged warnings —
        // a single failed step shouldn't abort the others.
        if let Err(e) = self
            .session_manager
            .inject_env_file(session.id, &managed_env, Some(&project_ctx))
            .await
        {
            tracing::warn!("refresh_sandbox: inject_env_file failed: {}", e);
        }
        if let Err(e) = self
            .session_manager
            .inject_skill_file(session.id, &session.ai_provider)
            .await
        {
            tracing::warn!("refresh_sandbox: inject_skill_file failed: {}", e);
        }

        // Re-run the shared injector so skill archives, MCP JSON, and secret
        // values reflect the latest state of the definition / secret tables.
        // Users hit "refresh" specifically when they've rotated a secret or
        // updated a skill — skipping this would leave the sandbox stale.
        if let (Some(secret_service), Some(definition_service)) = (
            self.secret_service.as_ref(),
            self.definition_service.as_ref(),
        ) {
            use temps_agents::services::sandbox_injector;
            let mcp_slugs = sandbox_injector::parse_slug_array(session.mcp_servers_config.as_ref());
            let skill_slugs = sandbox_injector::parse_slug_array(session.skills_config.as_ref());
            if !mcp_slugs.is_empty() || !skill_slugs.is_empty() {
                match secret_service.resolve_secrets().await {
                    Ok(secrets) => {
                        let fs = crate::services::workspace_sandbox_fs::WorkspaceSandboxFs {
                            sm: self.session_manager.clone(),
                            session_id: session.id,
                        };
                        if let Err(e) = sandbox_injector::inject(
                            &fs,
                            definition_service.clone(),
                            session.project_id,
                            &mcp_slugs,
                            &skill_slugs,
                            &secrets,
                            &session.ai_provider,
                        )
                        .await
                        {
                            tracing::warn!("refresh_sandbox: skill/MCP reinjection failed: {}", e);
                        }
                    }
                    Err(e) => tracing::warn!(
                        "refresh_sandbox: failed to resolve secrets for reinjection: {}",
                        e
                    ),
                }
            }
        }

        if let Err(e) = self
            .setup_git_credentials(session.id, session.user_id, git_creds.as_ref())
            .await
        {
            tracing::warn!("refresh_sandbox: setup_git_credentials failed: {}", e);
        }

        // Refresh the in-sandbox credential daemon's env file so it
        // picks up the rotated TEMPS_API_TOKEN (deployment tokens are
        // re-issued on every refresh per `issue_session_token`). Without
        // this, the daemon would still hold the old token and start
        // 401'ing once it expires.
        if let Err(e) = self
            .write_credential_daemon_env(session.id, &temps_api_url, &session_token)
            .await
        {
            tracing::warn!("refresh_sandbox: write_credential_daemon_env failed: {}", e);
        }

        // Sync cached branch with actual /workspace HEAD.
        self.sync_current_branch(session.id, session.branch_name.as_deref())
            .await;

        // Surface a system message so the user sees the refresh happened.
        let _ = self
            .workspace_service
            .append_message(SendMessageRequest {
                session_id,
                role: "system".to_string(),
                content: "Sandbox refreshed: skill, env, and deployment token reloaded."
                    .to_string(),
                metadata: None,
            })
            .await;

        Ok(())
    }

    pub async fn execute_message(
        &self,
        session_id: i32,
        user_message_content: String,
    ) -> Result<(), WorkspaceError> {
        // Serialize per session — concurrent `claude --continue` invocations
        // race on the CLI's on-disk session state and silently hang. The
        // second sender waits here until the first run finishes.
        let lock = self.lock_for(session_id).await;
        let _guard = lock.lock().await;

        let session = self.workspace_service.get_session(session_id).await?;

        if session.status == "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id,
                status: "closed".to_string(),
            });
        }

        // Check if sandbox exists for this session
        let sandbox_ready = self.session_manager.is_alive(session_id).await;

        // Defensive backfill: if the sandbox is already in-memory but the DB
        // row is missing the container id (e.g. an earlier run errored after
        // create_container but before update_session, or a server restart
        // adopted an existing container), persist it now so the UI stops
        // showing "not started".
        if sandbox_ready && session.sandbox_container_id.is_none() {
            if let Some(handle) = self.session_manager.get_handle(session_id).await {
                let _ = self
                    .workspace_service
                    .update_session(
                        session_id,
                        UpdateSessionFields {
                            sandbox_container_id: Some(handle.sandbox_id.clone()),
                            ..Default::default()
                        },
                    )
                    .await;
            }
        }

        if !sandbox_ready {
            // Surface a heartbeat event so the UI's "Thinking…" label tells
            // the user we're provisioning, not just sitting silent. Hard
            // wall-clock timeout on the whole setup pipeline so we never
            // hang the chat on a stuck git clone or stuck setup exec.
            let _ = self
                .workspace_service
                .append_message(SendMessageRequest {
                    session_id,
                    role: "ai_event".to_string(),
                    content: r#"{"type":"system","subtype":"setup","message":"Provisioning sandbox (clone repo, build container, inject skill files)…"}"#.to_string(),
                    metadata: None,
                })
                .await;

            const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
            match tokio::time::timeout(SETUP_TIMEOUT, self.initialize_sandbox(&session)).await {
                Ok(r) => r?,
                Err(_) => {
                    return Err(WorkspaceError::SandboxCreationFailed {
                        session_id,
                        reason: format!(
                            "Sandbox provisioning exceeded {}s timeout",
                            SETUP_TIMEOUT.as_secs()
                        ),
                    });
                }
            }
        }

        // --- Pre-run hygiene ---
        //
        // 1. Zombie sweep: kill any leftover `claude` processes from a prior
        //    run that was force-killed or orphaned by a server restart. Two
        //    claudes writing to the same jsonl corrupt it. This is always
        //    safe — if there's nothing to kill, pkill is a no-op.
        self.session_manager
            .kill_session_processes(
                session_id,
                "^claude ",
                temps_agents::sandbox::KillSignal::Kill,
            )
            .await;

        // 2. Repair pass: if this session is marked dirty (prior cancel or
        //    timeout), the on-disk claude jsonl may have a dangling tool_use
        //    or a truncated last line. Walk the file and fix it before the
        //    next --continue invocation. Best-effort: if anything goes wrong
        //    we clear the dirty flag and fall through to the --continue
        //    fallback below.
        let is_dirty = self.dirty_sessions.read().await.contains(&session_id);
        if is_dirty {
            if let Err(e) = self.repair_session_jsonl(session_id).await {
                tracing::warn!(
                    "repair_session_jsonl failed for session {}: {}. \
                     --continue may fail; will fall back to fresh session.",
                    session_id,
                    e
                );
            }
            self.dirty_sessions.write().await.remove(&session_id);
        }

        // Build the prompt — on first message we may inject workflow memory,
        // subsequent messages use --continue so we don't need prior history.
        let is_first = self.session_manager.is_first_message(session_id).await;

        // Workspace chat sessions don't have a workflow agent_id, so memory
        // injection is a no-op for them. Workflow runs (future code path)
        // will pass a real agent_id and tags here.
        let final_prompt = self
            .build_chat_prompt(
                &user_message_content,
                is_first,
                None, // workspace chat: no agent_id
                session.project_id,
                vec![], // no trigger-specific tags
            )
            .await;

        // Resolve the effective model: session-level override takes
        // precedence, then the provider's default_model from settings.
        let effective_model = self
            .resolve_effective_model(session.ai_model.as_deref(), &session.ai_provider)
            .await;

        // Run claude with a fallback path: if --continue fails because the
        // jsonl is unrepairable (tool_use/tool_result mismatch), delete the
        // jsonl and retry once without --continue. User loses prior context
        // but at least gets an answer this turn.
        let (result, buffer) = self
            .run_claude_with_fallback(
                session_id,
                &final_prompt,
                !is_first,
                &session.ai_provider,
                effective_model.as_deref(),
            )
            .await;

        // Mark first message sent regardless of success
        self.session_manager
            .mark_first_message_sent(session_id)
            .await;

        match result {
            Ok(exec_result) => {
                // Save a final assistant summary message
                let full_output = buffer.lock().await.clone();
                let summary = extract_final_result(&full_output)
                    .unwrap_or_else(|| "(no result text)".to_string());

                self.workspace_service
                    .append_message(SendMessageRequest {
                        session_id,
                        role: "assistant".to_string(),
                        content: summary,
                        metadata: Some(serde_json::json!({
                            "exit_code": exec_result.exit_code,
                        })),
                    })
                    .await?;

                // Parse token usage from stream-json
                let (tokens_in, tokens_out) = parse_token_usage(&full_output);
                if tokens_in.is_some() || tokens_out.is_some() {
                    let _ = self
                        .workspace_service
                        .update_session(
                            session_id,
                            UpdateSessionFields {
                                tokens_input: Some(session.tokens_input + tokens_in.unwrap_or(0)),
                                tokens_output: Some(
                                    session.tokens_output + tokens_out.unwrap_or(0),
                                ),
                                ..Default::default()
                            },
                        )
                        .await;
                }

                // Sync the cached branch_name with whatever /workspace HEAD
                // points at now — the AI may have switched branches mid-turn.
                self.sync_current_branch(session_id, session.branch_name.as_deref())
                    .await;

                Ok(())
            }
            Err(e) => {
                // User-initiated cancels are handled by the HTTP cancel_run
                // handler, which writes a single terminal "Run cancelled by
                // user." system+assistant pair. Writing more messages here
                // produces the duplicate cascade the UI used to show.
                let is_cancel = self.drain_cancel.read().await.contains(&session_id)
                    || matches!(
                        &e,
                        WorkspaceError::AiCliFailed { reason, .. }
                            if reason == "Run cancelled by user"
                    );
                if is_cancel {
                    return Err(e);
                }

                // Save BOTH a system breadcrumb and an assistant-role message.
                // The assistant message is what the UI watches to clear its
                // "Thinking…" indicator — without it the spinner spins forever
                // on any executor failure.
                let error_text = format!("Error: {}", e);
                let _ = self
                    .workspace_service
                    .append_message(SendMessageRequest {
                        session_id,
                        role: "system".to_string(),
                        content: error_text.clone(),
                        metadata: None,
                    })
                    .await;
                let _ = self
                    .workspace_service
                    .append_message(SendMessageRequest {
                        session_id,
                        role: "assistant".to_string(),
                        content: error_text,
                        metadata: Some(serde_json::json!({
                            "error": true,
                            "error_kind": format!("{:?}", e).split('{').next().unwrap_or("Unknown").trim().to_string(),
                        })),
                    })
                    .await;
                Err(e)
            }
        }
    }

    /// Run claude once with the given `continue_conversation` flag. Returns
    /// the exec result plus the collected stdout buffer. Handles cancel +
    /// timeout + process-tree kill internally.
    async fn run_claude_once(
        &self,
        session_id: i32,
        prompt: &str,
        continue_conversation: bool,
        provider: &str,
        model: Option<&str>,
    ) -> (
        Result<temps_agents::sandbox::SandboxExecResult, WorkspaceError>,
        Arc<Mutex<String>>,
    ) {
        // OpenCode's `run [message..]` tries to lstat the first positional arg
        // as a path, which fails with ENAMETOOLONG on long agent prompts. Write
        // the prompt to a temp file inside the sandbox and have `build_chat_cmd`
        // reference it via `$(cat ...)` so the prompt never hits the filesystem
        // as a path component.
        if provider == "opencode" {
            if let Err(e) = self
                .session_manager
                .write_file(session_id, "/tmp/.temps-prompt", prompt.as_bytes(), 0o644)
                .await
            {
                tracing::warn!(
                    "run_claude_once: failed to write prompt file for opencode session {}: {}",
                    session_id,
                    e
                );
            }
        }

        let env = std::collections::HashMap::new();
        let cmd =
            self.session_manager
                .build_chat_cmd(prompt, 25, continue_conversation, provider, model);

        let buffer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let workspace_service_for_callback = self.workspace_service.clone();

        let on_output: OnEventCallback = {
            let buffer = buffer.clone();
            Arc::new(move |line: String| {
                let buffer = buffer.clone();
                let ws_service = workspace_service_for_callback.clone();
                Box::pin(async move {
                    {
                        let mut b = buffer.lock().await;
                        b.push_str(&line);
                        b.push('\n');
                    }
                    let _ = ws_service
                        .append_message(SendMessageRequest {
                            session_id,
                            role: "ai_event".to_string(),
                            content: line,
                            metadata: None,
                        })
                        .await;
                })
            })
        };

        let cancel_token = CancellationToken::new();
        {
            self.active_runs
                .write()
                .await
                .insert(session_id, cancel_token.clone());
        }

        const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
        let exec_fut = self
            .session_manager
            .exec(session_id, cmd, env, Some(on_output));

        let result = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                self.dirty_sessions.write().await.insert(session_id);
                Err(WorkspaceError::AiCliFailed {
                    session_id,
                    reason: "Run cancelled by user".to_string(),
                })
            }
            r = tokio::time::timeout(EXEC_TIMEOUT, exec_fut) => match r {
                Ok(inner) => inner,
                Err(_) => {
                    self.dirty_sessions.write().await.insert(session_id);
                    self.session_manager
                        .kill_session_processes(session_id, "^claude ", temps_agents::sandbox::KillSignal::Term)
                        .await;
                    let sm = self.session_manager.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        sm.kill_session_processes(session_id, "^claude ", temps_agents::sandbox::KillSignal::Kill).await;
                    });
                    Err(WorkspaceError::AiCliFailed {
                        session_id,
                        reason: format!(
                            "AI CLI run exceeded {}s timeout and was aborted",
                            EXEC_TIMEOUT.as_secs()
                        ),
                    })
                }
            }
        };

        self.active_runs.write().await.remove(&session_id);

        (result, buffer)
    }

    /// Run claude with a fallback: if the first attempt with `--continue`
    /// fails because the jsonl is corrupted (tool_use/tool_result mismatch),
    /// delete the jsonl and retry once without `--continue`. On the retry
    /// the user loses prior context but at least gets a response. Returns
    /// the final result + the buffer from whichever attempt succeeded
    /// (or the last failed attempt).
    async fn run_claude_with_fallback(
        &self,
        session_id: i32,
        prompt: &str,
        continue_conversation: bool,
        provider: &str,
        model: Option<&str>,
    ) -> (
        Result<temps_agents::sandbox::SandboxExecResult, WorkspaceError>,
        Arc<Mutex<String>>,
    ) {
        let (first_result, first_buffer) = self
            .run_claude_once(session_id, prompt, continue_conversation, provider, model)
            .await;

        // Only consider a fallback if we actually tried to --continue. A
        // fresh-session run has nothing to fall back to.
        if !continue_conversation {
            return (first_result, first_buffer);
        }

        // Decide if this looks like a jsonl corruption error. Heuristics:
        //   - exec returned Ok but exit_code != 0, AND
        //   - stdout contains a known mismatch marker
        // We intentionally keep this conservative — false positives here
        // nuke the user's conversation history, which is worse than
        // surfacing the raw error.
        let is_mismatch = match &first_result {
            Ok(r) if r.exit_code != 0 => {
                let out = first_buffer.lock().await;
                let s = out.as_str();
                s.contains("tool_use") && (s.contains("tool_result") || s.contains("mismatch"))
                    || s.contains("unexpected `tool_use` block")
                    || s.contains("messages.0.content")
            }
            _ => false,
        };

        if !is_mismatch {
            return (first_result, first_buffer);
        }

        tracing::warn!(
            "run_claude_with_fallback: session {} jsonl appears corrupted, \
             deleting and retrying without --continue",
            session_id
        );

        // Tell the user what's happening so they understand why context is lost.
        let _ = self
            .workspace_service
            .append_message(SendMessageRequest {
                session_id,
                role: "system".to_string(),
                content: "Previous conversation state was corrupted by an interrupted run. \
                          Starting a fresh Claude session — prior context is lost."
                    .to_string(),
                metadata: None,
            })
            .await;

        // Nuke every .jsonl in the claude projects dir for this sandbox.
        // We don't know the exact filename, so shotgun-delete them all.
        self.session_manager
            .kill_session_processes(
                session_id,
                "^claude ",
                temps_agents::sandbox::KillSignal::Kill,
            )
            .await;
        let claude_projects_dir = format!(
            "{}/.claude/projects/{}",
            SANDBOX_HOME,
            SANDBOX_WORK_DIR.replace('/', "-")
        );
        let delete_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("rm -f {}/*.jsonl 2>/dev/null; exit 0", claude_projects_dir),
        ];
        let _ = self
            .session_manager
            .exec(session_id, delete_cmd, HashMap::new(), None)
            .await;

        // Second attempt: fresh session (no --continue).
        let (second_result, second_buffer) = self
            .run_claude_once(session_id, prompt, false, provider, model)
            .await;
        (second_result, second_buffer)
    }

    /// Eagerly provision the sandbox for a session if it isn't already
    /// running. Used on session start/reopen so the terminal tab has a
    /// live container to attach to without waiting for a first chat
    /// message. No-op if the sandbox is already alive.
    pub async fn ensure_sandbox(&self, session_id: i32) -> Result<(), WorkspaceError> {
        let lock = self.lock_for(session_id).await;
        let _guard = lock.lock().await;

        let session = self.workspace_service.get_session(session_id).await?;
        if session.status == "closed" {
            return Err(WorkspaceError::SessionNotActive {
                session_id,
                status: "closed".to_string(),
            });
        }
        if self.session_manager.is_alive(session_id).await {
            return Ok(());
        }

        const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
        match tokio::time::timeout(SETUP_TIMEOUT, self.initialize_sandbox(&session)).await {
            Ok(r) => r,
            Err(_) => Err(WorkspaceError::SandboxCreationFailed {
                session_id,
                reason: format!(
                    "Sandbox provisioning exceeded {}s timeout",
                    SETUP_TIMEOUT.as_secs()
                ),
            }),
        }
    }

    /// Initialize the sandbox for this session: clone repo, create container, inject skill.
    async fn initialize_sandbox(
        &self,
        session: &temps_entities::workspace_sessions::Model,
    ) -> Result<(), WorkspaceError> {
        // Load the project
        let project = projects::Entity::find_by_id(session.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WorkspaceError::ProjectNotFound {
                project_id: session.project_id,
            })?;

        // Create temp work directory. The work_dir is bind-mounted into the
        // sandbox and persists across container recreation, so on reopen/retry
        // we want to *reuse* the existing checkout rather than re-cloning. A
        // dir is considered "already initialized" if it contains a `.git`
        // subdirectory — anything else (e.g. an empty leftover dir) is treated
        // as fresh and may be cloned into.
        let work_dir = std::env::temp_dir().join(format!("workspace-{}", session.id));
        tokio::fs::create_dir_all(&work_dir).await?;
        let work_dir_already_initialized = work_dir.join(".git").exists();

        // Clone the repo if git provider is configured. There are three cases:
        //   1. Neither branch_name nor base_branch_name set → clone project's main.
        //   2. branch_name set, no base → clone that branch directly (existing remote branch).
        //   3. base_branch_name + branch_name set → clone the base branch, then
        //      create a new local branch off it. The new branch only lives in
        //      the sandbox until something pushes it.
        let (branch_to_clone, new_branch_to_create) = match (
            session.base_branch_name.as_deref(),
            session.branch_name.as_deref(),
        ) {
            (Some(base), Some(new_branch)) => (base, Some(new_branch)),
            (None, Some(branch)) => (branch, None),
            (None, None) => (project.main_branch.as_str(), None),
            // base set without branch_name should have been rejected by
            // create_session validation; treat defensively as "use base".
            (Some(base), None) => (base, None),
        };
        if work_dir_already_initialized {
            tracing::info!(
                "Reusing existing work_dir for session {} ({}) — skipping git clone",
                session.id,
                work_dir.display()
            );
        } else if let Some(connection_id) = project.git_provider_connection_id {
            self.git_provider_manager
                .clone_repository(
                    connection_id,
                    &project.repo_owner,
                    &project.repo_name,
                    &work_dir,
                    Some(branch_to_clone),
                )
                .await
                .map_err(|e| WorkspaceError::SandboxCreationFailed {
                    session_id: session.id,
                    reason: format!("Git clone failed: {}", e),
                })?;

            // If we need to fork a new branch off the cloned base, do it now
            // before the sandbox container is created. The branch lives only
            // in the local clone.
            if let Some(new_branch) = new_branch_to_create {
                let work_dir_for_branch = work_dir.clone();
                let new_branch_owned = new_branch.to_string();
                tokio::task::spawn_blocking(move || {
                    temps_git::services::git_ops::create_and_checkout_branch_at(
                        &work_dir_for_branch,
                        &new_branch_owned,
                    )
                })
                .await
                .map_err(|e| WorkspaceError::SandboxCreationFailed {
                    session_id: session.id,
                    reason: format!("Branch creation task panicked: {}", e),
                })?
                .map_err(|e| WorkspaceError::SandboxCreationFailed {
                    session_id: session.id,
                    reason: format!("Could not create branch: {}", e),
                })?;
                tracing::info!(
                    "Created branch '{}' off '{}' for workspace session {}",
                    new_branch,
                    branch_to_clone,
                    session.id
                );
            }
        } else {
            // No git provider — write a placeholder README so the sandbox
            // has something to mount
            tokio::fs::write(
                work_dir.join("README.md"),
                format!(
                    "# {}\n\nNo git provider configured for this project.\n",
                    project.name
                ),
            )
            .await?;
        }

        // Issue a deployment token for this session so the sandbox can
        // call back to the Temps API (analytics, errors, deploys, etc.)
        let session_token = self
            .issue_session_token(session.project_id, session.id)
            .await?;

        // Build env vars to inject at container creation. Workspace chat
        // sessions don't have an associated workflow slug, so memory writes
        // from the chat sandbox will fail until we add a chat-scoped memory
        // model. Workflow runs use a different code path that DOES set the slug.
        let (provider_id, auth_type, decrypted_credential) = self.resolve_ai_credentials().await?;
        let temps_api_url = self.get_temps_api_url().await;
        let env_vars = WorkspaceSessionManager::build_env_vars_with_workflow(
            &temps_api_url,
            &session_token,
            &provider_id,
            &auth_type,
            decrypted_credential.as_deref(),
            Some(session.project_id),
            None, // chat sessions: no workflow scope
        );

        // Create the sandbox with per-session resource overrides (when set
        // on the workspace_sessions row). Each is None → provider default.
        self.session_manager
            .create_sandbox(
                session.id,
                &session.public_id,
                session.project_id,
                work_dir.clone(),
                env_vars,
                session.cpu_milli.map(|m| m as f32 / 1000.0),
                session.memory_limit_mb,
                session.pids_limit,
            )
            .await?;

        // Inject the Temps platform skill file
        let _ = self
            .session_manager
            .inject_skill_file(session.id, &session.ai_provider)
            .await;

        // Inject per-session skills + MCP servers + secrets via the shared
        // agent-sandbox injector. Skipped cleanly if the agents plugin is not
        // loaded (both services are None in that case).
        if let (Some(secret_service), Some(definition_service)) = (
            self.secret_service.as_ref(),
            self.definition_service.as_ref(),
        ) {
            use temps_agents::services::sandbox_injector;
            let mcp_slugs = sandbox_injector::parse_slug_array(session.mcp_servers_config.as_ref());
            let skill_slugs = sandbox_injector::parse_slug_array(session.skills_config.as_ref());

            match secret_service.resolve_secrets().await {
                Ok(secrets) => {
                    let fs = crate::services::workspace_sandbox_fs::WorkspaceSandboxFs {
                        sm: self.session_manager.clone(),
                        session_id: session.id,
                    };
                    match sandbox_injector::inject(
                        &fs,
                        definition_service.clone(),
                        session.project_id,
                        &mcp_slugs,
                        &skill_slugs,
                        &secrets,
                        &session.ai_provider,
                    )
                    .await
                    {
                        Ok(s) => {
                            tracing::info!(
                                "Workspace session {}: injected {} MCP, {} skill, {} env-secret, {} file-secret",
                                session.id,
                                s.mcp_count,
                                s.skill_count,
                                s.env_secret_count,
                                s.file_secret_count
                            );
                            for slug in s.unresolved_mcp_slugs {
                                tracing::warn!(
                                    "Workspace session {}: MCP slug '{}' not found — skipped",
                                    session.id,
                                    slug
                                );
                            }
                            for slug in s.unresolved_skill_slugs {
                                tracing::warn!(
                                    "Workspace session {}: skill slug '{}' not found — skipped",
                                    session.id,
                                    slug
                                );
                            }
                        }
                        Err(e) => tracing::warn!(
                            "Workspace session {}: sandbox injector failed: {}",
                            session.id,
                            e
                        ),
                    }
                }
                Err(e) => tracing::warn!(
                    "Workspace session {}: failed to resolve secrets: {}",
                    session.id,
                    e
                ),
            }
        }

        // Seed ~/.claude.json so the terminal's first `claude` launch
        // doesn't block on the onboarding/theme picker. Best-effort — a
        // failure here shouldn't abort sandbox creation.
        if let Err(e) = self.session_manager.seed_claude_config(session.id).await {
            tracing::warn!(
                "Failed to seed claude config for session {}: {}",
                session.id,
                e
            );
        }

        // Seed ~/.codex/config.toml so the terminal's first `codex` launch
        // doesn't block on the "Do you trust the contents of this directory?"
        // picker. Applies even when the active provider isn't codex, because
        // the user can open a codex tab at any point. Best-effort.
        if let Err(e) = self.session_manager.seed_codex_config(session.id).await {
            tracing::warn!(
                "Failed to seed codex config for session {}: {}",
                session.id,
                e
            );
        }

        // Drop every configured provider's credential into the sandbox via
        // the catalog dispatcher. For env-var flavors (`ApiKey`) this merges
        // env vars into `~/.env` — for file-based flavors (`OauthToken`,
        // `ConfigFile`) the bytes land directly on disk.
        //
        // Seeding ALL providers (not just the active one) matters because the
        // terminal UI lets the user open a tab per CLI. If we only seeded the
        // default, a user whose default is codex would see "Not logged in"
        // when they open a claude tab even though they've configured Claude
        // credentials. Disjoint seed paths keep the writes conflict-free.
        let mut managed_env: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        managed_env.extend(self.seed_all_configured_providers(session.id).await);

        // Linked external services (DATABASE_URL, REDIS_URL, ...)
        match self
            .external_service_manager
            .get_project_service_environment_variables(session.project_id)
            .await
        {
            Ok(by_service) => {
                for (_service_id, vars) in by_service {
                    for (k, v) in vars {
                        managed_env.insert(k, v);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load linked-service env vars for project {}: {}",
                    session.project_id,
                    e
                );
            }
        }

        // Git authentication is now handled by the in-sandbox credential
        // daemon (see temps-git-credential). We deliberately do NOT
        // inject GH_TOKEN/GITHUB_TOKEN/GITLAB_TOKEN/GL_TOKEN into the
        // user-visible environment any more — every git operation gets
        // a fresh per-repo per-op scoped token from the daemon, and the
        // long-lived installation token never crosses the user/process
        // boundary. The daemon's deployment token lives in a 0600
        // env file owned by uid 1001 (`temps-git`) which user code (uid
        // 1000) cannot read.
        //
        // Tradeoff: `gh` and `glab` CLIs that previously read these env
        // vars will need to be re-authenticated explicitly inside the
        // sandbox if the user wants them. This is by design — those
        // CLIs need API-level scopes (issues, PRs, releases) that the
        // git-clone-only narrow tokens don't grant. Deferring that to
        // a follow-up that wires `gh` through a separate scoped flow.
        //
        // Kept as `None` for call-site stability; setup_git_credentials
        // ignores the value now that the daemon owns this path.
        let git_creds: Option<(String, String)> = None;

        // Always inject the global CLAUDE.md (with project context) even
        // when there are no env vars to write — the agent still needs to
        // know which project this sandbox belongs to.
        let project_ctx = crate::services::session_manager::ProjectContext {
            id: project.id,
            slug: &project.slug,
            name: &project.name,
            repo_owner: &project.repo_owner,
            repo_name: &project.repo_name,
            branch: branch_to_clone,
        };

        if let Err(e) = self
            .session_manager
            .inject_env_file(session.id, &managed_env, Some(&project_ctx))
            .await
        {
            tracing::warn!("Failed to inject ~/.env into session {}: {}", session.id, e);
        }

        // Write the CLI's persisted-login files (`~/.temps/.contexts.json`,
        // `~/.temps/.secrets`, and `~/.config/temps-cli-nodejs/config.json`)
        // so `bunx @temps-sdk/cli` is authenticated even when the user runs
        // it outside `. ~/.env` (e.g. from a fresh shell tab the AI just
        // opened).
        if let Err(e) = self
            .write_cli_auth_files(session.id, session.user_id, &temps_api_url, &session_token)
            .await
        {
            tracing::warn!(
                "Failed to seed CLI auth files for session {}: {}",
                session.id,
                e
            );
        }

        // Wire git config (user.name + user.email from the session owner)
        // and force HTTPS rewrites. Per-op credentials come from the
        // in-sandbox credential daemon, NOT from a long-lived
        // `~/.git-credentials` file — see `setup_git_credentials` above
        // for the full migration story.
        if let Err(e) = self
            .setup_git_credentials(session.id, session.user_id, git_creds.as_ref())
            .await
        {
            tracing::warn!(
                "Failed to set up git credentials for session {}: {}",
                session.id,
                e
            );
        }

        // Provision the credential daemon's env file. This is the only
        // place the workspace deployment token is written into the
        // sandbox; it lives at /etc/temps/credential-daemon.env, mode
        // 0600, owned by `temps-git` (uid 1001). User code (uid 1000)
        // cannot read it. The daemon picks up the file on next start
        // (the entrypoint supervises with a 5s back-off, so even if we
        // lose this race the daemon attaches within seconds).
        if let Err(e) = self
            .write_credential_daemon_env(session.id, &temps_api_url, &session_token)
            .await
        {
            tracing::warn!(
                "Failed to write credential daemon env for session {}: {}",
                session.id,
                e
            );
        }

        // Install the memory script. The script itself enforces that
        // TEMPS_WORKFLOW_SLUG is set before any memory operation, so chat
        // sessions get a clear error if the AI tries to use memory.
        let _ = self.session_manager.inject_memory_script(session.id).await;

        // Update session with sandbox container ID
        let handle = self.session_manager.get_handle(session.id).await;
        if let Some(handle) = handle {
            let _ = self
                .workspace_service
                .update_session(
                    session.id,
                    UpdateSessionFields {
                        sandbox_container_id: Some(handle.sandbox_id.clone()),
                        work_dir: Some(work_dir.to_string_lossy().to_string()),
                        ..Default::default()
                    },
                )
                .await;
        }

        tracing::info!("Initialized sandbox for workspace session {}", session.id);
        Ok(())
    }

    /// Configure git inside the sandbox so the AI can pull/push without
    /// having to know about tokens.
    ///
    /// What this writes:
    /// 1. `git config --global user.email` and `user.name` — set to the
    ///    session owner's email/name so commits are attributed correctly.
    /// 2. Default branch + pull.rebase preferences.
    /// 3. `url.https://<host>/.insteadOf git@<host>:` so any accidental
    ///    SSH-style URL gets rewritten to HTTPS, which the credential
    ///    daemon can serve.
    ///
    /// What this NO LONGER does (deliberate security change): write
    /// `~/.git-credentials` containing a long-lived token. That's been
    /// replaced by the in-sandbox credential daemon (see
    /// `temps-git-credential` and the system-wide
    /// `credential.helper=temps-git-credential-helper` set in the
    /// sandbox image). Tokens never touch user-owned disk any more.
    async fn setup_git_credentials(
        &self,
        session_id: i32,
        user_id: i32,
        git_creds: Option<&(String, String)>,
    ) -> Result<(), WorkspaceError> {
        // Look up the session owner's name + email for git config.
        let user = users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?;

        // Build a single bash command that sets everything up. Using one
        // exec keeps the round-trip count low and atomic from the AI's POV.
        // We single-quote the values and escape embedded single quotes via
        // the standard `'\''` shell idiom — same as inject_env_file.
        let shell_quote = |v: &str| v.replace('\'', "'\\''");

        let mut script = String::from("set -e\n");
        script.push_str(&format!("mkdir -p {}\n", SANDBOX_HOME));

        if let Some(u) = user.as_ref() {
            script.push_str(&format!(
                "git config --global user.email '{}'\n",
                shell_quote(&u.email)
            ));
            script.push_str(&format!(
                "git config --global user.name '{}'\n",
                shell_quote(&u.name)
            ));
        }

        // Sensible defaults regardless of provider.
        script.push_str("git config --global init.defaultBranch main\n");
        script.push_str("git config --global pull.rebase false\n");

        // Force HTTPS rewrites for both providers — the credential daemon
        // serves HTTPS, never SSH. We don't have provider-side info here
        // any more (since git_creds is always None now), so we set both
        // rewrites unconditionally; harmless when one isn't used.
        script.push_str("git config --global url.https://github.com/.insteadOf git@github.com:\n");
        script.push_str("git config --global url.https://gitlab.com/.insteadOf git@gitlab.com:\n");

        // Idempotent cleanup of a stale `~/.git-credentials` left over
        // from older session lifetimes that ran before this change.
        // Without this, a re-opened session with a stale named volume
        // would still see the old long-lived token sitting on disk.
        script.push_str(&format!(
            "rm -f {home}/.git-credentials\n",
            home = SANDBOX_HOME,
        ));

        // Make sure everything is owned by the sandbox user.
        script.push_str(&format!(
            "chown -R {chown} {home}/.gitconfig 2>/dev/null || true\n",
            chown = SANDBOX_CHOWN,
            home = SANDBOX_HOME,
        ));

        let cmd = vec!["sh".to_string(), "-c".to_string(), script];
        self.session_manager
            .exec(session_id, cmd, HashMap::new(), None)
            .await?;

        tracing::debug!(
            "Configured git for session {} (user_email={}, credentials_via_daemon=true)",
            session_id,
            user.as_ref().map(|u| u.email.as_str()).unwrap_or("<none>"),
        );
        // `git_creds` is intentionally unused — kept in the signature for
        // call-site stability while the daemon migration lands.
        let _ = git_creds;
        Ok(())
    }

    /// Write the credential daemon's env file inside the sandbox.
    ///
    /// The daemon (running as `temps-git`, uid 1001) reads
    /// `/etc/temps/credential-daemon.env` at startup to learn the
    /// control-plane URL and the per-session deployment token it should
    /// authenticate with when minting scoped git credentials.
    ///
    /// Security properties:
    /// - File mode is `0600` and owned by `temps-git:temps-git`. User
    ///   code (uid 1000 = `temps`) cannot read it.
    /// - The Dockerfile pre-creates the file at image build time with
    ///   the correct ownership and mode. This function overwrites the
    ///   *contents* by exec'ing as `temps-git` directly — that uid is
    ///   the only one that can open the file in the 0700 parent dir.
    ///   We deliberately do NOT do this as root, because the sandbox
    ///   container drops `CAP_DAC_OVERRIDE` (only `CHOWN` and `FOWNER`
    ///   are kept), so root inside the container *cannot* bypass the
    ///   0700 directory permission. Trying to do so silently fails
    ///   with EACCES, which is exactly the bug that bit us before this
    ///   refactor.
    /// - Token is identical to `TEMPS_API_TOKEN` in `~/.env` — but where
    ///   the env var is readable by the user (and gives them the same
    ///   ability to call the mint endpoint themselves), the env file is
    ///   not. The asymmetry is the security improvement: user code can
    ///   ask for a credential via the daemon's IPC socket but cannot
    ///   bypass the daemon and call the mint endpoint directly with a
    ///   different (project_id-spoofing) host header.
    ///
    /// Idempotent: safe to call again on session refresh — overwrite of
    /// the existing file preserves perms/ownership because we never
    /// recreate it.
    async fn write_credential_daemon_env(
        &self,
        session_id: i32,
        api_url: &str,
        api_token: &str,
    ) -> Result<(), WorkspaceError> {
        let shell_quote = |v: &str| v.replace('\'', "'\\''");

        // Trailing newline matters — daemon's `lines()` parser skips
        // the last line if it doesn't end with one.
        let body = format!(
            "# Managed by temps. Do not edit.\nTEMPS_API_URL='{}'\nTEMPS_API_TOKEN='{}'\n",
            shell_quote(api_url),
            shell_quote(api_token),
        );

        // Overwrite-in-place. The file already exists at image build
        // time with the correct ownership/mode; the heredoc cat keeps
        // both. `umask 077` is belt-and-braces in case a future image
        // change ever recreates the file from scratch.
        let script = format!(
            "set -e
umask 077
cat > /etc/temps/credential-daemon.env <<'TEMPS_DAEMON_ENV_EOF'
{body}TEMPS_DAEMON_ENV_EOF
chmod 0600 /etc/temps/credential-daemon.env"
        );

        let cmd = vec!["sh".to_string(), "-c".to_string(), script];
        // Run as `temps-git` (uid 1001) — the only uid that can open
        // the file in /etc/temps/. See doc comment above for the
        // CapDrop=ALL rationale that rules out running as root.
        self.session_manager
            .exec_as_user(session_id, "1001:1001", cmd, HashMap::new(), None)
            .await?;

        tracing::debug!(
            "Wrote credential daemon env for session {} (api_url={})",
            session_id,
            api_url
        );

        // Now kick the daemon off as `temps-git`. The sandbox container
        // sets `no-new-privileges:true`, which blocks an in-container
        // supervisor from `sudo`-ing into uid 1001 — so we launch via
        // `docker exec --user` (a Docker API call, which is *not*
        // subject to no-new-privileges) and detach with `setsid`.
        //
        // Idempotency: if the daemon is already running we don't want
        // to spawn a second one, so the launcher first checks for an
        // existing socket. The daemon's bind() also fails-fast on
        // collision, but the pre-check keeps the logs clean.
        let launch_script = "set -e
if [ -S /run/temps-git/git.sock ]; then
    # Daemon already running — env-file refresh path. Send SIGHUP so
    # it picks up the new token. The daemon doesn't currently handle
    # SIGHUP — but a future revision can without changing this caller.
    pkill -HUP -u temps-git temps-git-cred 2>/dev/null || true
    exit 0
fi
# setsid + redirect detaches from the docker exec lifecycle so this
# process survives `start_exec`'s wait. Logs go to syslog-ish stdout
# of the container which `docker logs` surfaces.
setsid /usr/local/bin/temps-git-credential-daemon \
    >> /tmp/temps-git-credential-daemon.log \
    2>&1 < /dev/null &
disown 2>/dev/null || true";
        let launch_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            launch_script.to_string(),
        ];
        if let Err(e) = self
            .session_manager
            .exec_as_user(session_id, "1001:1001", launch_cmd, HashMap::new(), None)
            .await
        {
            tracing::warn!(
                "Failed to launch credential daemon for session {}: {}",
                session_id,
                e
            );
        }
        Ok(())
    }

    /// Resolve the active AI provider's credentials from the global settings
    /// table. Returns:
    ///   - `provider_id`: which CLI to use (`claude_cli`, `codex_cli`,
    ///     `opencode`, …) — drives catalog dispatch downstream.
    ///   - `auth_type`: which flavor of credential the user picked
    ///     (`subscription`, `api_key`, `config_file`, …).
    ///   - `decrypted`: the plaintext credential bytes if the user has saved
    ///     one, else `None` (the sandbox still launches; the agent will fail
    ///     authentication when it tries to use the CLI).
    ///
    /// Reads through `AgentSandboxSettings::provider_config`, which falls back
    /// to the legacy flat fields (`auth_type`/`api_key_encrypted`) when the
    /// settings row predates the multi-provider schema — so existing users
    /// don't need to re-enter their key.
    async fn resolve_ai_credentials(
        &self,
    ) -> Result<(String, String, Option<Vec<u8>>), WorkspaceError> {
        let settings_row = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        let sandbox = settings_row
            .as_ref()
            .and_then(|row| row.data.get("agent_sandbox"))
            .and_then(|v| {
                serde_json::from_value::<temps_core::AgentSandboxSettings>(v.clone()).ok()
            })
            .unwrap_or_default();

        let provider_id = if sandbox.default_provider.is_empty() {
            "claude_cli".to_string()
        } else {
            sandbox.default_provider.clone()
        };

        let provider_cfg = sandbox.provider_config(&provider_id);
        let auth_type = if provider_cfg.auth_type.is_empty() {
            // Fall back to the catalog default flavor for this provider.
            temps_agents::ai_cli::find_provider(&provider_id)
                .map(|p| p.default_flavor().id.to_string())
                .unwrap_or_else(|| "api_key".to_string())
        } else {
            provider_cfg.auth_type.clone()
        };

        let decrypted = match provider_cfg.credentials_encrypted.as_deref() {
            Some(encrypted) => match self.encryption_service.decrypt(encrypted) {
                Ok(bytes) => Some(bytes),
                Err(e) => {
                    tracing::warn!(
                        "resolve_ai_credentials: decrypt failed for provider {}: {}",
                        provider_id,
                        e
                    );
                    None
                }
            },
            None => None,
        };

        Ok((provider_id, auth_type, decrypted))
    }

    /// Resolve the effective model for a workspace message. Returns the
    /// session-level override if set, otherwise the provider's configured
    /// `default_model` from the global settings. Returns `None` when neither
    /// is set (the CLI will use its own built-in default).
    async fn resolve_effective_model(
        &self,
        session_model: Option<&str>,
        provider_id: &str,
    ) -> Option<String> {
        // Session-level override takes precedence.
        if let Some(m) = session_model {
            if !m.is_empty() {
                return Some(m.to_string());
            }
        }
        // Fall back to the provider's default_model from settings.
        let settings_row = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();
        let sandbox = settings_row
            .as_ref()
            .and_then(|row| row.data.get("agent_sandbox"))
            .and_then(|v| {
                serde_json::from_value::<temps_core::AgentSandboxSettings>(v.clone()).ok()
            })
            .unwrap_or_default();
        sandbox
            .provider_config(provider_id)
            .default_model
            .filter(|m| !m.is_empty())
    }

    /// Resolve credentials for **every** configured provider, not just the
    /// active/default one. Workspaces let the user open a terminal tab per
    /// provider (claude tab, codex tab, shell tab), so each provider that has
    /// saved credentials needs its file/env landing in the sandbox — otherwise
    /// tabs for non-default providers show "Not logged in".
    ///
    /// Returns one entry per provider that has a non-empty credential, with
    /// decryption already performed. Providers whose credential fails to
    /// decrypt are logged and skipped (rather than failing the whole bootstrap
    /// — one bad entry shouldn't lock the user out of every CLI).
    ///
    /// The returned list is unordered; callers should seed them in any
    /// order since each provider's seed paths are disjoint
    /// (`~/.claude/.credentials.json`, `~/.codex/auth.json`,
    /// `~/.local/share/opencode/auth.json`, …).
    async fn resolve_all_ai_credentials(
        &self,
    ) -> Result<Vec<(String, String, Vec<u8>)>, WorkspaceError> {
        let settings_row = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await?;

        let sandbox = settings_row
            .as_ref()
            .and_then(|row| row.data.get("agent_sandbox"))
            .and_then(|v| {
                serde_json::from_value::<temps_core::AgentSandboxSettings>(v.clone()).ok()
            })
            .unwrap_or_default();

        // Union of every provider id the catalog knows about, because the
        // legacy fields on `AgentSandboxSettings` are surfaced through
        // `provider_config("claude_cli")` even when the `providers` map
        // doesn't have an explicit entry. Iterating the catalog ensures we
        // cover that legacy-migration path too.
        let mut out = Vec::new();
        for entry in temps_agents::ai_cli::PROVIDER_CATALOG {
            let cfg = sandbox.provider_config(entry.id);
            let encrypted = match cfg.credentials_encrypted.as_deref() {
                Some(e) if !e.is_empty() => e,
                _ => continue,
            };
            let auth_type = if cfg.auth_type.is_empty() {
                entry.default_flavor().id.to_string()
            } else {
                cfg.auth_type.clone()
            };
            match self.encryption_service.decrypt(encrypted) {
                Ok(bytes) => out.push((entry.id.to_string(), auth_type, bytes)),
                Err(e) => tracing::warn!(
                    "resolve_all_ai_credentials: decrypt failed for provider {}: {} — skipping",
                    entry.id,
                    e
                ),
            }
        }
        Ok(out)
    }

    /// Seed every configured provider's credential into the sandbox and
    /// return any env vars that should be merged into `~/.env` (for ApiKey
    /// flavors). Failures for individual providers are logged and skipped.
    ///
    /// Shared by `initialize_sandbox` (first boot) and `refresh_sandbox`
    /// (token rotation / env rewrite) so both paths produce the same on-disk
    /// state.
    async fn seed_all_configured_providers(
        &self,
        session_id: i32,
    ) -> std::collections::HashMap<String, String> {
        let mut env = std::collections::HashMap::new();
        let all = match self.resolve_all_ai_credentials().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "seed_all_configured_providers: resolve failed for session {}: {}",
                    session_id,
                    e
                );
                return env;
            }
        };
        if all.is_empty() {
            tracing::debug!(
                "seed_all_configured_providers: no credentials configured for any provider (session {})",
                session_id
            );
        }
        for (provider_id, auth_type, creds) in all {
            match self
                .session_manager
                .seed_provider_credentials(session_id, &provider_id, &auth_type, &creds)
                .await
            {
                Ok(provider_env) => env.extend(provider_env),
                Err(e) => tracing::warn!(
                    "Failed to seed {} credentials for session {}: {}",
                    provider_id,
                    session_id,
                    e
                ),
            }
        }
        env
    }

    /// Resolve the URL the sandbox should use to call back to the Temps API.
    ///
    /// Resolution order:
    ///   1. `TEMPS_INTERNAL_API_URL` env var — escape hatch for CI/dev when
    ///      you want the sandbox to bypass the public URL (e.g. talk to a
    ///      local server via `host.docker.internal`).
    ///   2. `external_url` from platform settings — the public URL real users
    ///      hit (e.g. `https://temps.example.com`). This is what the in-sandbox
    ///      CLI's persisted login points at, so commands like
    ///      `bunx @temps-sdk/cli` work the same as if the user ran them on
    ///      their host.
    ///   3. `http://host.docker.internal:3000` — last-resort fallback for
    ///      single-machine dev where the user hasn't configured `external_url`.
    async fn get_temps_api_url(&self) -> String {
        if let Ok(env_url) = std::env::var("TEMPS_INTERNAL_API_URL") {
            return env_url;
        }
        match self.platform_config_service.get_external_url().await {
            Ok(Some(url)) if !url.is_empty() => url,
            Ok(_) => "http://host.docker.internal:3000".to_string(),
            Err(e) => {
                tracing::warn!(
                    "Failed to read external_url from platform settings, falling back to host.docker.internal: {}",
                    e
                );
                "http://host.docker.internal:3000".to_string()
            }
        }
    }

    /// Persist the deployment token in the in-sandbox CLI's auth files so
    /// `bunx @temps-sdk/cli` is authenticated without the user needing to
    /// `. ~/.env` first. We write two files, both mode 0600:
    ///
    /// - `~/.temps/.contexts.json` — the CLI's multi-instance auth store
    ///   (cf. `temps-cli/src/config/contexts.ts`). Contains a single
    ///   `workspace` context flagged `isActive: true`, pointing at the
    ///   public API URL with the session's deployment token as `apiKey`.
    /// - `~/.temps/.secrets` — the legacy single-instance secrets file
    ///   (cf. `temps-cli/src/config/store.ts`). Contains
    ///   `temps_api_key`, `temps_user_id`, `temps_email` so the CLI's
    ///   `credentials.getApiKey()` resolves on the fallback path too.
    ///
    /// Called from both `initialize_sandbox` (first boot) and
    /// `refresh_sandbox` (token rotation) so both paths produce the same
    /// on-disk state.
    async fn write_cli_auth_files(
        &self,
        session_id: i32,
        user_id: i32,
        api_url: &str,
        api_token: &str,
    ) -> Result<(), WorkspaceError> {
        // Look up the session owner's email — the CLI surfaces it in
        // `temps configure` output for users to confirm they're logged in
        // as the right account.
        let user = users::Entity::find_by_id(user_id)
            .one(self.db.as_ref())
            .await?;
        let email = user.as_ref().map(|u| u.email.as_str()).unwrap_or("");

        let temps_dir = format!("{}/.temps", SANDBOX_HOME);
        let contexts_path = format!("{}/.contexts.json", temps_dir);
        let secrets_path = format!("{}/.secrets", temps_dir);
        // Conf (the CLI's config library) writes to
        // `$XDG_CONFIG_HOME/<projectName>-nodejs/config.json` on Linux. The
        // CLI sets `projectName: 'temps-cli'`, so the on-disk path is
        // `~/.config/temps-cli-nodejs/config.json`.
        let cli_config_dir = format!("{}/.config/temps-cli-nodejs", SANDBOX_HOME);
        let cli_config_path = format!("{}/config.json", cli_config_dir);

        // mkdir -p both target directories inside the container.
        let _ = self
            .session_manager
            .exec(
                session_id,
                vec!["mkdir".into(), "-p".into(), temps_dir, cli_config_dir],
                std::collections::HashMap::new(),
                None,
            )
            .await;

        // Contexts file (multi-instance store).
        let key_prefix: String = api_token.chars().take(8).collect();
        let contexts = serde_json::json!([{
            "name": "workspace",
            "url": api_url,
            "apiKey": api_token,
            "email": email,
            "keyPrefix": key_prefix,
            "isActive": true,
        }]);
        let contexts_bytes =
            serde_json::to_vec_pretty(&contexts).map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("write_cli_auth_files: contexts serialize failed: {}", e),
            })?;
        self.session_manager
            .write_file(session_id, &contexts_path, &contexts_bytes, 0o600)
            .await?;

        // Secrets file (legacy single-instance store). Plain `key="value"`
        // lines, mirroring `temps-cli/src/config/store.ts:saveSecrets`.
        let escape = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
        let mut secrets_body = String::from("# Temps CLI secrets - DO NOT SHARE THIS FILE\n");
        secrets_body.push_str(&format!("temps_api_key=\"{}\"\n", escape(api_token)));
        secrets_body.push_str(&format!("temps_user_id=\"{}\"\n", user_id));
        secrets_body.push_str(&format!("temps_email=\"{}\"\n", escape(email)));
        self.session_manager
            .write_file(session_id, &secrets_path, secrets_body.as_bytes(), 0o600)
            .await?;

        // CLI config file. `apiUrl` comes from the platform's external URL
        // (resolved by `get_temps_api_url`) so the in-sandbox CLI dials the
        // same public endpoint a user would. Shape mirrors the sample
        // `~/.config/temps-cli-nodejs/config.json` produced by `temps configure`.
        let cli_config = serde_json::json!({
            "apiUrl": api_url,
            "outputFormat": "table",
            "colorEnabled": true,
        });
        let cli_config_bytes =
            serde_json::to_vec_pretty(&cli_config).map_err(|e| WorkspaceError::AiCliFailed {
                session_id,
                reason: format!("write_cli_auth_files: cli config serialize failed: {}", e),
            })?;
        self.session_manager
            .write_file(session_id, &cli_config_path, &cli_config_bytes, 0o644)
            .await?;

        tracing::debug!(
            "Seeded CLI auth files for session {} (api_url={}, email={})",
            session_id,
            api_url,
            email
        );
        Ok(())
    }

    /// Issue a deployment token scoped to this project for the workspace session.
    /// This is the token the sandbox uses to authenticate back to the Temps API
    /// (e.g. for `temps errors list`, `temps analytics`, etc).
    async fn issue_session_token(
        &self,
        project_id: i32,
        session_id: i32,
    ) -> Result<String, WorkspaceError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::deployment_tokens;

        let token_name = format!("workspace-session-{}", session_id);

        // Drop any pre-existing token with this name so refresh/recreate cycles
        // don't trip the unique-name conflict in create_token. Best-effort: if
        // the lookup or delete fails we'll surface the original Conflict from
        // create_token below.
        if let Ok(Some(existing)) = deployment_tokens::Entity::find()
            .filter(deployment_tokens::Column::ProjectId.eq(project_id))
            .filter(deployment_tokens::Column::Name.eq(&token_name))
            .one(self.db.as_ref())
            .await
        {
            if let Err(e) = self
                .deployment_token_service
                .delete_token(project_id, existing.id)
                .await
            {
                tracing::warn!(
                    "Failed to delete stale workspace session token {} for project {}: {}",
                    existing.id,
                    project_id,
                    e
                );
            }
        }

        // Token lifetime: 6 hours. Long enough for a realistic interactive
        // Claude/agent session, short enough that a leaked env-var token
        // stops being useful within one work session. The stale-token
        // cleanup above (delete_token on name collision) reissues on every
        // new session, so users don't hit expiry mid-session in practice.
        //
        // Permissions: still FullAccess (`*`) because the memory and
        // workspace handlers gate on the admin Permission enum, not on
        // granular deployment-token permissions. Fine-grained scoping is
        // tracked as Phase 2 of the security hardening plan.
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(6);
        let request = CreateDeploymentTokenRequest {
            name: token_name,
            environment_id: None,
            deployment_id: None,
            permissions: Some(vec!["*".to_string()]),
            expires_at: Some(expires_at),
        };

        let response = self
            .deployment_token_service
            .create_token(project_id, None, request)
            .await
            .map_err(|e| WorkspaceError::SandboxCreationFailed {
                session_id,
                reason: format!("Failed to issue deployment token: {}", e),
            })?;

        Ok(response.token)
    }
}

/// Extract the final result text from Claude stream-json output.
/// Returns the content of the `{"type":"result","result":"..."}` line if present.
/// Extract the final assistant-visible text from a CLI's stream-json output.
/// Handles all three providers we run in chat sessions:
///   - Claude: `{"type":"result","result":"..."}` OR
///     `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}`
///   - Codex:  `{"type":"item.started|item.completed","item":{"type":"agent_message","text":"..."}}`
///   - OpenCode: `{"type":"text","part":{"text":"..."}}`
///
/// Strategy: walk the output forward, collect every assistant text segment
/// from any recognised event, and join them. If none match we return None so
/// the caller can decide how to surface the empty case.
fn extract_final_result(output: &str) -> Option<String> {
    // Claude's `result` event is authoritative when present — it's the final
    // consolidated answer after all assistant turns. Prefer it.
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if value.get("type").and_then(|v| v.as_str()) == Some("result") {
                if let Some(result) = value.get("result").and_then(|v| v.as_str()) {
                    if !result.is_empty() {
                        return Some(result.to_string());
                    }
                }
            }
        }
    }

    // Otherwise accumulate assistant text from whichever provider shape we see.
    //
    // OpenCode's `--format json` with `--continue` replays every historical
    // text part in the session before emitting the current turn (it filters
    // events by sessionID, not messageID). So a naive accumulator produces
    // a transcript of every prior answer. Track the latest opencode messageID
    // separately and only keep text parts that share it.
    let mut parts: Vec<String> = Vec::new();
    let mut opencode_texts: Vec<(String, String)> = Vec::new(); // (messageID, text)
    let mut latest_opencode_message_id: Option<String> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event_type = match v.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };
        match event_type {
            // Claude stream-json assistant turn.
            "assistant" => {
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
            // Codex: agent message can arrive on started OR completed. Dedupe
            // against what we've already captured so newer builds that emit
            // both don't produce a doubled answer.
            "item.started" | "item.completed" => {
                if let Some(item) = v.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("agent_message") {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() && !parts.iter().any(|p| p == text) {
                                parts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            // OpenCode `--format json`: one `text` event per assistant segment.
            // `part.messageID` groups segments by turn; `--continue` replays
            // historical turns too, so we stash every text part and its
            // messageID, remember the most-recently-seen messageID, and at
            // the end only keep parts whose messageID matches that latest
            // turn. Parts without a messageID (shouldn't happen per the
            // opencode SDK types, but defensively) are ignored.
            "text" => {
                let part = match v.get("part") {
                    Some(p) => p,
                    None => continue,
                };
                let text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    continue;
                }
                if let Some(message_id) = part.get("messageID").and_then(|m| m.as_str()) {
                    latest_opencode_message_id = Some(message_id.to_string());
                    opencode_texts.push((message_id.to_string(), text.to_string()));
                }
            }
            _ => {}
        }
    }

    // Fold opencode's filtered-by-messageID text parts into the output.
    if let Some(latest_id) = latest_opencode_message_id {
        for (mid, text) in opencode_texts {
            if mid == latest_id {
                parts.push(text);
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Parse cumulative token usage from a stream-json output buffer.
fn parse_token_usage(output: &str) -> (Option<i32>, Option<i32>) {
    let mut input_tokens: Option<i32> = None;
    let mut output_tokens: Option<i32> = None;

    for line in output.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(usage) = value
                .get("usage")
                .or_else(|| value.get("message").and_then(|m| m.get("usage")))
            {
                if let Some(n) = usage.get("input_tokens").and_then(|v| v.as_i64()) {
                    input_tokens = Some(n as i32);
                }
                if let Some(n) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                    output_tokens = Some(n as i32);
                }
            }
        }
    }

    (input_tokens, output_tokens)
}

/// Repair a potentially-corrupted Claude CLI session jsonl.
///
/// Claude persists conversation state to `~/.claude/projects/<hash>/<id>.jsonl`,
/// one JSON object per line. When we SIGKILL claude mid-turn, the file can
/// end with:
///   (a) a truncated last line (partial JSON) — drop it
///   (b) an `assistant` turn containing a `tool_use` block with no matching
///       `tool_result` on a later line — inject a synthetic tool_result
///
/// Both cases make `claude --continue` fail with "tool_use/tool_result
/// mismatch" or a JSON parse error. This function reads the raw bytes,
/// walks line by line, and rewrites the file if needed.
///
/// Returns true if the file was modified, false if it was already clean.
pub(crate) fn repair_claude_jsonl(raw: &[u8]) -> (Vec<u8>, bool) {
    let text = match std::str::from_utf8(raw) {
        Ok(s) => s,
        Err(_) => {
            // Non-UTF8 garbage — bail out, leave file alone. Caller will
            // likely fall back to fresh session on the next --continue error.
            return (raw.to_vec(), false);
        }
    };

    // Split into lines, drop trailing empty/malformed ones.
    let mut valid: Vec<serde_json::Value> = Vec::new();
    let mut had_trailing_garbage = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => valid.push(v),
            Err(_) => {
                // Partial write — discard this line and anything after it.
                had_trailing_garbage = true;
                break;
            }
        }
    }

    // Scan the valid lines and look for dangling tool_use blocks (assistant
    // turn with a tool_use whose id never appears as a tool_result).
    //
    // We collect all tool_use ids, then all tool_result ids, and diff.
    let mut tool_use_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in &valid {
        collect_tool_use_ids(entry, &mut tool_use_ids);
        collect_tool_result_ids(entry, &mut tool_result_ids);
    }

    let dangling: Vec<String> = tool_use_ids
        .into_iter()
        .filter(|id| !tool_result_ids.contains(id))
        .collect();

    let needs_rewrite = had_trailing_garbage || !dangling.is_empty();
    if !needs_rewrite {
        return (raw.to_vec(), false);
    }

    // For each dangling tool_use, append a synthetic user-turn tool_result
    // marking it as an interrupted run. Claude's conversation format expects
    // tool_results to appear in a subsequent user message.
    for tool_use_id in dangling {
        let synthetic = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": "Run cancelled by user before tool finished.",
                    "is_error": true,
                }]
            }
        });
        valid.push(synthetic);
    }

    let mut out = String::new();
    for entry in &valid {
        // serde_json::to_string never fails on a Value we just parsed.
        if let Ok(line) = serde_json::to_string(entry) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    (out.into_bytes(), true)
}

fn collect_tool_use_ids(entry: &serde_json::Value, out: &mut Vec<String>) {
    // Walk any `content` array looking for `type: "tool_use"` blocks.
    if let Some(message) = entry.get("message") {
        if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                        out.push(id.to_string());
                    }
                }
            }
        }
    }
}

fn collect_tool_result_ids(entry: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
    if let Some(message) = entry.get("message") {
        if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                        out.insert(id.to_string());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_final_result() {
        let output = r#"{"type":"system","model":"claude-sonnet"}
{"type":"assistant","message":{"content":"thinking..."}}
{"type":"result","result":"The fix is to rename foo to bar","duration_ms":5000}
"#;
        let result = extract_final_result(output);
        assert_eq!(result, Some("The fix is to rename foo to bar".to_string()));
    }

    #[test]
    fn test_extract_final_result_missing() {
        let output = r#"{"type":"system"}
{"type":"assistant"}
"#;
        assert_eq!(extract_final_result(output), None);
    }

    #[test]
    fn test_extract_final_result_empty() {
        assert_eq!(extract_final_result(""), None);
    }

    #[test]
    fn test_extract_final_result_opencode_text_event() {
        // OpenCode emits `{"type":"text","part":{"text":"..."}}` per segment.
        // Before the fix, no `result` event was present and the chat UI
        // showed "(no result text)" even though opencode had answered.
        let output = "\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m1\",\"text\":\"3 + 3 = 6\"}}\n";
        assert_eq!(extract_final_result(output), Some("3 + 3 = 6".to_string()));
    }

    #[test]
    fn test_extract_final_result_opencode_skips_historical_messages_on_continue() {
        // `opencode run --continue --format json` replays every historical
        // text part in the session before emitting the new turn. We group by
        // messageID and only surface parts from the latest turn; otherwise the
        // user sees a cumulative transcript ("3+3=6, 2+2=4, 4+4=8") when they
        // only asked for the latest answer.
        let output = "\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m1\",\"text\":\"3+3 = 6\"}}\n\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m2\",\"text\":\"2+2 = 4\"}}\n\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m3\",\"text\":\"4+4 = 8\"}}\n";
        assert_eq!(extract_final_result(output), Some("4+4 = 8".to_string()));
    }

    #[test]
    fn test_extract_final_result_opencode_multiple_parts_same_message() {
        // A single turn can emit multiple text parts (e.g. before/after a
        // tool call). All parts with the latest messageID should be joined.
        let output = "\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m_old\",\"text\":\"prior turn\"}}\n\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m_new\",\"text\":\"Part one.\"}}\n\
{\"type\":\"text\",\"part\":{\"type\":\"text\",\"messageID\":\"m_new\",\"text\":\"Part two.\"}}\n";
        assert_eq!(
            extract_final_result(output),
            Some("Part one.\n\nPart two.".to_string())
        );
    }

    #[test]
    fn test_extract_final_result_codex_agent_message_completed() {
        let output = "\
{\"type\":\"thread.started\",\"thread_id\":\"abc\"}\n\
{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"The answer is 6.\"}}\n\
{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n";
        assert_eq!(
            extract_final_result(output),
            Some("The answer is 6.".to_string())
        );
    }

    #[test]
    fn test_extract_final_result_codex_agent_message_started() {
        // gpt-5-codex streams the answer on item.started, not item.completed.
        let output = "\
{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"Reading input...\"}}\n";
        assert_eq!(
            extract_final_result(output),
            Some("Reading input...".to_string())
        );
    }

    #[test]
    fn test_extract_final_result_codex_dedupes_started_and_completed() {
        // Newer codex builds may emit the same message on both events — only
        // surface it once in the final chat reply.
        let output = "\
{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"Hi.\"}}\n\
{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Hi.\"}}\n";
        assert_eq!(extract_final_result(output), Some("Hi.".to_string()));
    }

    #[test]
    fn test_extract_final_result_claude_assistant_text_fallback() {
        // No `result` event (e.g. claude was interrupted mid-turn); fall back
        // to the last assistant text block so the user still sees something.
        let output = "\
{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial answer\"}]}}\n";
        assert_eq!(
            extract_final_result(output),
            Some("partial answer".to_string())
        );
    }

    #[test]
    fn test_extract_final_result_prefers_result_over_assistant() {
        // When both are present, the authoritative `result` wins.
        let output = "\
{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"thinking\"}]}}\n\
{\"type\":\"result\",\"result\":\"final answer\"}\n";
        assert_eq!(
            extract_final_result(output),
            Some("final answer".to_string())
        );
    }

    #[test]
    fn test_parse_token_usage_top_level() {
        let output = r#"{"type":"result","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let (input, output_t) = parse_token_usage(output);
        assert_eq!(input, Some(100));
        assert_eq!(output_t, Some(50));
    }

    #[test]
    fn test_parse_token_usage_nested_in_message() {
        let output =
            r#"{"type":"assistant","message":{"usage":{"input_tokens":200,"output_tokens":75}}}"#;
        let (input, output_t) = parse_token_usage(output);
        assert_eq!(input, Some(200));
        assert_eq!(output_t, Some(75));
    }

    #[test]
    fn test_parse_token_usage_none() {
        let output = r#"{"type":"system","model":"claude"}"#;
        let (input, output_t) = parse_token_usage(output);
        assert_eq!(input, None);
        assert_eq!(output_t, None);
    }

    #[test]
    fn test_parse_token_usage_takes_last() {
        let output = r#"{"usage":{"input_tokens":10,"output_tokens":5}}
{"usage":{"input_tokens":20,"output_tokens":15}}"#;
        let (input, output_t) = parse_token_usage(output);
        assert_eq!(input, Some(20));
        assert_eq!(output_t, Some(15));
    }

    // ── build_chat_prompt_with_memory ────────────────────────────────────────
    //
    // We test the free-function variant directly so we don't need to spin up
    // the full MessageExecutor with all its dependencies. The MessageExecutor
    // method just delegates to this function.

    use async_trait::async_trait;
    use temps_core::{WorkflowMemoryError, WorkflowMemoryFact};

    /// Minimal in-memory `WorkflowMemoryProvider` for prompt-building tests.
    ///
    /// Tests used to mock `WorkflowMemoryService` via Sea-ORM `MockDatabase`,
    /// which was over-specified: we don't care about SQL here, we care about
    /// the trait contract. Swapping to the trait makes these tests portable
    /// to any provider that satisfies the `temps-memory` eval harness.
    struct FakeMemoryProvider {
        facts: Vec<WorkflowMemoryFact>,
    }

    #[async_trait]
    impl WorkflowMemoryProvider for FakeMemoryProvider {
        async fn load_for_trigger(
            &self,
            _project_id: i32,
            _agent_id: i32,
            _relevant_tags: Vec<String>,
            _limit: usize,
        ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
            Ok(self.facts.clone())
        }

        fn render_for_prompt(&self, facts: &[WorkflowMemoryFact]) -> String {
            if facts.is_empty() {
                return String::new();
            }
            let mut out = String::from("## Things you've learned about this from past runs\n\n");
            for f in facts {
                out.push_str(&format!("- {}\n", f.fact));
            }
            out
        }
    }

    fn make_test_fact(id: i64, fact: &str) -> WorkflowMemoryFact {
        WorkflowMemoryFact {
            id,
            fact: fact.to_string(),
            confidence: 0.9,
            times_used: 0,
        }
    }

    fn provider_with(facts: Vec<WorkflowMemoryFact>) -> FakeMemoryProvider {
        FakeMemoryProvider { facts }
    }

    #[tokio::test]
    async fn test_build_chat_prompt_no_memory_provider_returns_user_content() {
        let result = build_chat_prompt_with_memory(None, "hello", true, Some(5), 10, vec![]).await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_build_chat_prompt_no_agent_id_returns_user_content() {
        let memory = provider_with(vec![]);
        let result = build_chat_prompt_with_memory(
            Some(&memory),
            "hello",
            true,
            None, // no agent
            10,
            vec![],
        )
        .await;
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn test_build_chat_prompt_subsequent_message_skips_memory() {
        // Even if memory has facts, is_first_message=false skips them.
        let memory = provider_with(vec![make_test_fact(1, "should not appear")]);
        let result = build_chat_prompt_with_memory(
            Some(&memory),
            "follow-up",
            false, // not first message
            Some(5),
            10,
            vec![],
        )
        .await;
        assert_eq!(result, "follow-up");
    }

    #[tokio::test]
    async fn test_build_chat_prompt_first_message_with_memory_includes_section() {
        let memory = provider_with(vec![make_test_fact(1, "OAuth state cookie missing")]);

        let result = build_chat_prompt_with_memory(
            Some(&memory),
            "fix the bug",
            true,
            Some(5),
            10,
            vec!["error_group_id:42".to_string()],
        )
        .await;

        assert!(
            result.contains("Things you've learned"),
            "memory section should be present"
        );
        assert!(result.contains("OAuth state cookie missing"));
        assert!(result.contains("## Current request"));
        assert!(result.contains("fix the bug"));
        // The memory section should come BEFORE the user request
        let memory_pos = result.find("Things you've learned").unwrap();
        let request_pos = result.find("## Current request").unwrap();
        assert!(memory_pos < request_pos);
    }

    #[tokio::test]
    async fn test_build_chat_prompt_empty_memory_returns_user_content() {
        // Provider is set but returns no facts.
        let memory = provider_with(vec![]);
        let result =
            build_chat_prompt_with_memory(Some(&memory), "hello", true, Some(5), 10, vec![]).await;
        // No memory rows → no section → user content as-is
        assert_eq!(result, "hello");
    }

    /// ADR-010 swap-in proof (PR 3.4).
    ///
    /// The consumer holds `Option<&dyn WorkflowMemoryProvider>`, never a
    /// concrete type. This test demonstrates that two **different** provider
    /// impls — a tag-aware ranker and a pass-through one — both wire into
    /// the exact same consumer code path and produce the expected prompt
    /// shape. If this test stops compiling, it means the consumer has grown
    /// a dependency on a concrete type (a boundary regression); if it stops
    /// passing, the trait contract has drifted.
    ///
    /// This is the live counterpart to the `temps-memory` eval harness: the
    /// harness pins the contract *as viewed by the trait*, and this test
    /// pins the contract *as viewed by the consumer*.
    #[tokio::test]
    async fn test_message_executor_accepts_any_trait_impl() {
        /// Tag-aware impl: returns only facts whose tags overlap the request.
        struct TagMatchingProvider {
            facts: Vec<(WorkflowMemoryFact, Vec<String>)>,
        }

        #[async_trait]
        impl WorkflowMemoryProvider for TagMatchingProvider {
            async fn load_for_trigger(
                &self,
                _project_id: i32,
                _agent_id: i32,
                relevant_tags: Vec<String>,
                _limit: usize,
            ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
                Ok(self
                    .facts
                    .iter()
                    .filter(|(_, tags)| tags.iter().any(|t| relevant_tags.contains(t)))
                    .map(|(f, _)| f.clone())
                    .collect())
            }
            fn render_for_prompt(&self, facts: &[WorkflowMemoryFact]) -> String {
                if facts.is_empty() {
                    return String::new();
                }
                let mut out = String::from("## Things you've learned\n\n");
                for f in facts {
                    out.push_str(&format!("- {}\n", f.fact));
                }
                out
            }
        }

        let tag_matching = TagMatchingProvider {
            facts: vec![
                (
                    make_test_fact(1, "oauth fact"),
                    vec!["error_group:42".into()],
                ),
                (make_test_fact(2, "unrelated fact"), vec!["other".into()]),
            ],
        };
        let pass_through = provider_with(vec![make_test_fact(3, "passthrough fact")]);

        // Both providers are held as `Arc<dyn WorkflowMemoryProvider>` —
        // this is the shape the real wiring uses (see plugin.rs). If the
        // trait stopped being object-safe, this line would fail to compile.
        let providers: Vec<Arc<dyn WorkflowMemoryProvider>> =
            vec![Arc::new(tag_matching), Arc::new(pass_through)];

        // First provider returns the tag-matched fact; second returns its
        // single fact regardless of tags. Both go through the same consumer
        // code path. If either trait method were bypassed by the consumer,
        // this test would fail to compile or diverge in behavior.
        let result0 = build_chat_prompt_with_memory(
            Some(providers[0].as_ref()),
            "fix it",
            true,
            Some(5),
            10,
            vec!["error_group:42".into()],
        )
        .await;
        assert!(
            result0.contains("oauth fact"),
            "tag-matching provider must surface oauth fact; got: {result0}"
        );
        assert!(
            !result0.contains("unrelated fact"),
            "unrelated fact must be filtered out; got: {result0}"
        );

        let result1 = build_chat_prompt_with_memory(
            Some(providers[1].as_ref()),
            "fix it",
            true,
            Some(5),
            10,
            vec![],
        )
        .await;
        assert!(
            result1.contains("passthrough fact"),
            "pass-through provider must surface its fact; got: {result1}"
        );
    }

    #[tokio::test]
    async fn test_build_chat_prompt_load_error_degrades_gracefully() {
        // If the provider errors, the executor must not fail the turn — the
        // user message goes through without the memory section.
        struct FailingProvider;

        #[async_trait]
        impl WorkflowMemoryProvider for FailingProvider {
            async fn load_for_trigger(
                &self,
                _: i32,
                _: i32,
                _: Vec<String>,
                _: usize,
            ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
                Err(WorkflowMemoryError::new("boom"))
            }
            fn render_for_prompt(&self, _: &[WorkflowMemoryFact]) -> String {
                unreachable!("render is not called when load errors")
            }
        }

        let provider = FailingProvider;
        let result =
            build_chat_prompt_with_memory(Some(&provider), "hello", true, Some(5), 10, vec![])
                .await;
        assert_eq!(result, "hello");
    }
}
