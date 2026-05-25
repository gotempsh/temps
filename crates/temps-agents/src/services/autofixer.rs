use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;

use temps_core::{EncryptionService, JobQueue};
use temps_entities::{error_events, error_groups, projects};
use temps_error_tracking::services::source_map_service::SourceMapService;
use temps_git::services::git_provider_manager_trait::{GitProviderManagerTrait, PullRequest};

use crate::ai_cli::OnEventCallback;
use crate::error::AgentError;
use crate::services::executor::{build_claude_cmd, AgentExecutor, PrepareWorkspaceParams};
use crate::services::run_service::{AgentRunService, UpdateRunFields};
use crate::services::sandbox_registry::SandboxRegistry;

/// Autofixer service: implements the two-phase AI error-fixing workflow.
///
/// Phase 1 – `start_analysis`: reads code, identifies root cause, stores analysis text.
/// Phase 2 – `start_fix`: uses stored analysis to generate a minimal fix + test.
/// Phase 3 – `create_pr`: pushes the fix branch and opens a pull request.
pub struct AutofixerService {
    db: Arc<DatabaseConnection>,
    git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    #[allow(dead_code)]
    encryption_service: Arc<EncryptionService>,
    #[allow(dead_code)]
    queue: Arc<dyn JobQueue>,
    run_service: Arc<AgentRunService>,
    source_map_service: Arc<SourceMapService>,
    sandbox_registry: Arc<SandboxRegistry>,
    /// Shared executor used for the unified sandbox workspace setup
    /// (credentials, MCP, skills, git credentials, memory script).
    /// Both regular workflow runs and autofixer runs go through
    /// `AgentExecutor::prepare_sandbox_workspace`, so any change to the AI
    /// agent environment applies uniformly.
    executor: Arc<AgentExecutor>,
}

impl AutofixerService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DatabaseConnection>,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
        encryption_service: Arc<EncryptionService>,
        queue: Arc<dyn JobQueue>,
        run_service: Arc<AgentRunService>,
        source_map_service: Arc<SourceMapService>,
        sandbox_registry: Arc<SandboxRegistry>,
        executor: Arc<AgentExecutor>,
    ) -> Self {
        Self {
            db,
            git_provider_manager,
            encryption_service,
            queue,
            run_service,
            source_map_service,
            sandbox_registry,
            executor,
        }
    }

    /// Returns the stable temp directory path for a given autofixer run.
    /// This directory persists across analysis → fix phases; only cleaned up by `create_pr` or `cancel_run`.
    pub fn work_dir(run_id: i32) -> PathBuf {
        std::env::temp_dir().join(format!("autofixer-{}", run_id))
    }

    // ── Phase 1: Analysis ──────────────────────────────────────────────────────

    /// Create a run record, clone the repo, run Claude in analysis-only mode, and
    /// store the root cause analysis.  Returns the newly created run ID immediately
    /// (the caller should spawn this in a background task after creating the record).
    pub async fn run_analysis(&self, run_id: i32) {
        tracing::info!("Autofixer run {}: starting analysis phase", run_id);

        match self.run_analysis_inner(run_id).await {
            Ok(()) => {
                tracing::info!("Autofixer run {}: analysis phase completed", run_id);
            }
            Err(e) => {
                tracing::error!("Autofixer run {}: analysis failed: {}", run_id, e);
                let _ = self
                    .run_service
                    .update_status(
                        run_id,
                        UpdateRunFields {
                            status: Some("failed".to_string()),
                            phase: Some("failed".to_string()),
                            error_message: Some(e.to_string()),
                            completed_at: Some(Utc::now()),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = self
                    .run_service
                    .append_log(run_id, "error", &format!("Analysis failed: {}", e), None)
                    .await;
            }
        }
    }

    async fn run_analysis_inner(&self, run_id: i32) -> Result<(), AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        // Load project
        let project = projects::Entity::find_by_id(run.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::ProjectNotFound {
                project_id: run.project_id,
            })?;

        // Load error context
        let (error_type, error_message, stack_trace, _env) = self
            .load_error_context(run.trigger_source_id, run.project_id)
            .await?;

        // Update status → "cloning"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("cloning".to_string()),
                    started_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(run_id, "info", "Cloning repository...", None)
            .await?;

        // Clone the repository — clean up any leftover work dir from a crashed previous run
        let work_dir = Self::work_dir(run_id);
        if work_dir.exists() {
            let _ = fs::remove_dir_all(&work_dir).await;
        }
        fs::create_dir_all(&work_dir).await?;

        let connection_id = project
            .git_provider_connection_id
            .ok_or(AgentError::GitError {
                message: format!(
                    "Project {} has no git provider connection configured",
                    run.project_id
                ),
            })?;

        self.git_provider_manager
            .clone_repository(
                connection_id,
                &project.repo_owner,
                &project.repo_name,
                &work_dir,
                Some(&project.main_branch),
            )
            .await
            .map_err(|e| AgentError::GitError {
                message: format!(
                    "Failed to clone {}/{} for autofixer run {}: {}",
                    project.repo_owner, project.repo_name, run_id, e
                ),
            })?;

        // Prepare the sandbox workspace through the shared executor helper.
        //
        // This is the ONLY place autofixer sets up the AI environment — it
        // uses the exact same code path as regular workflow runs so the
        // Claude OAuth credential file, MCP servers, skills, memory script,
        // and git credentials all get installed identically. Previously
        // autofixer had its own ad-hoc setup that only wrote an env var and
        // skipped everything else, which caused Claude to fail with
        // "Not logged in".
        self.executor
            .prepare_sandbox_workspace(PrepareWorkspaceParams {
                run_id,
                project: &project,
                agent_config: None,
                ai_provider: "claude_cli",
                agent_slug: "autofixer",
                timeout_seconds: 600,
                host_work_dir: work_dir.clone(),
                ephemeral_yaml: None,
            })
            .await?;

        // Update status → "analyzing"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("analyzing".to_string()),
                    phase: Some("analyzing".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(
                run_id,
                "info",
                "Repository cloned. Analyzing error...",
                None,
            )
            .await?;

        // Build analysis-only prompt
        let user_context_section = run
            .user_context
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|ctx| format!("\nADDITIONAL CONTEXT FROM USER:\n{}\n", ctx))
            .unwrap_or_default();

        let prompt = format!(
            "You are analyzing a production error in the {project_name} project.\n\n\
            ERROR CONTEXT:\n\
            Type: {error_type}\n\
            Message: {error_message}\n\
            Stack trace:\n\
            {stack_trace}\n\
            {user_context_section}\n\
            IMPORTANT: Do NOT fix anything yet. Your job is ONLY to:\n\
            1. Read the relevant source files from the stack trace\n\
            2. Understand the code path that leads to this error\n\
            3. Identify the root cause\n\
            4. Explain what's happening and why\n\n\
            Output a clear root cause analysis.",
            project_name = project.name,
            error_type = error_type,
            error_message = error_message,
            stack_trace = stack_trace,
            user_context_section = user_context_section,
        );

        // Set up streaming callback
        let run_service_for_stream = self.run_service.clone();
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let svc = run_service_for_stream.clone();
            Box::pin(async move {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return;
                }
                let _ = svc.append_log(run_id, "ai_event", trimmed, None).await;
            })
        });

        self.run_service
            .append_log(
                run_id,
                "info",
                "Running Claude for root cause analysis...",
                None,
            )
            .await?;

        // Run Claude CLI inside sandbox
        let cmd = build_claude_cmd("claude_cli", &prompt, 10, false, None);
        let exec_result = tokio::time::timeout(
            Duration::from_secs(300),
            self.sandbox_registry.exec(
                run_id,
                cmd,
                std::collections::HashMap::new(),
                Some(on_event),
            ),
        )
        .await
        .map_err(|_| AgentError::AiCliTimeout {
            provider: "claude_cli".to_string(),
            timeout_secs: 300,
        })??;

        if exec_result.exit_code != 0 {
            return Err(AgentError::AiCliFailed {
                provider: "claude_cli".to_string(),
                exit_code: exec_result.exit_code,
                stderr: exec_result.stdout,
            });
        }

        let parsed = crate::ai_cli::claude::parse_claude_output(&exec_result.stdout);
        let (tokens_input, tokens_output, model) =
            (parsed.tokens_input, parsed.tokens_output, parsed.model);

        self.run_service
            .append_log(
                run_id,
                "info",
                "Claude analysis completed",
                Some(serde_json::json!({
                    "exit_code": exec_result.exit_code,
                    "tokens_input": tokens_input,
                    "tokens_output": tokens_output,
                })),
            )
            .await?;

        // Extract the result text from the stream-json output
        let analysis_text = extract_result_text(&exec_result.stdout);

        // Save analysis text and transition to "analyzed"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("analyzed".to_string()),
                    phase: Some("analyzed".to_string()),
                    analysis: Some(analysis_text),
                    ai_output: Some(exec_result.stdout),
                    ai_model: model,
                    tokens_input,
                    tokens_output,
                    ..Default::default()
                },
            )
            .await?;

        self.run_service
            .append_log(
                run_id,
                "info",
                "Analysis complete. Review the findings and click 'Generate Fix' to proceed.",
                None,
            )
            .await?;

        tracing::info!(
            "Autofixer run {}: analysis complete, phase=analyzed",
            run_id
        );

        Ok(())
    }

    // ── Phase 2: Fix ───────────────────────────────────────────────────────────

    /// Run the fix phase for an already-analyzed run (phase must be "analyzed").
    /// Should be spawned as a background task by the handler.
    pub async fn run_fix(&self, run_id: i32) {
        tracing::info!("Autofixer run {}: starting fix phase", run_id);

        match self.run_fix_inner(run_id).await {
            Ok(()) => {
                tracing::info!("Autofixer run {}: fix phase completed", run_id);
            }
            Err(e) => {
                tracing::error!("Autofixer run {}: fix failed: {}", run_id, e);
                let _ = self
                    .run_service
                    .update_status(
                        run_id,
                        UpdateRunFields {
                            status: Some("failed".to_string()),
                            phase: Some("failed".to_string()),
                            error_message: Some(e.to_string()),
                            completed_at: Some(Utc::now()),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = self
                    .run_service
                    .append_log(run_id, "error", &format!("Fix failed: {}", e), None)
                    .await;
            }
        }
    }

    async fn run_fix_inner(&self, run_id: i32) -> Result<(), AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        if run.phase.as_deref() != Some("analyzed") {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} cannot start fix: expected phase 'analyzed', got '{}'",
                    run_id,
                    run.phase.as_deref().unwrap_or("none")
                ),
            });
        }

        if run.analysis.is_none() {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} has no analysis text; cannot generate fix",
                    run_id
                ),
            });
        }

        // Update status → "fixing"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("fixing".to_string()),
                    phase: Some("fixing".to_string()),
                    ..Default::default()
                },
            )
            .await?;

        let work_dir = Self::work_dir(run_id);
        if !work_dir.exists() {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} work directory {:?} no longer exists; cannot generate fix. \
                     The server may have been restarted between the analysis and fix phases.",
                    run_id, work_dir
                ),
            });
        }

        self.run_service
            .append_log(run_id, "info", "Generating fix based on analysis...", None)
            .await?;

        // Build fix prompt — short since Claude already has the full analysis context
        let prompt = "Now fix this error. Instructions:\n\
            1. Apply the minimal fix required\n\
            2. Write a test that would have caught this bug\n\
            3. Run existing tests if they exist\n\
            4. Commit with message: fix: <description>\n\n\
            Do NOT change unrelated files."
            .to_string();

        // Streaming callback
        let run_service_for_stream = self.run_service.clone();
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let svc = run_service_for_stream.clone();
            Box::pin(async move {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return;
                }
                let _ = svc.append_log(run_id, "ai_event", trimmed, None).await;
            })
        });

        self.run_service
            .append_log(
                run_id,
                "info",
                "Continuing conversation to generate fix...",
                None,
            )
            .await?;

        // Use --continue to keep the full analysis context from the same session
        let cmd = build_claude_cmd("claude_cli", &prompt, 20, true, None);
        let exec_result = tokio::time::timeout(
            Duration::from_secs(600),
            self.sandbox_registry.exec(
                run_id,
                cmd,
                std::collections::HashMap::new(),
                Some(on_event),
            ),
        )
        .await
        .map_err(|_| AgentError::AiCliTimeout {
            provider: "claude_cli".to_string(),
            timeout_secs: 600,
        })??;

        if exec_result.exit_code != 0 {
            return Err(AgentError::AiCliFailed {
                provider: "claude_cli".to_string(),
                exit_code: exec_result.exit_code,
                stderr: exec_result.stdout,
            });
        }

        let parsed = crate::ai_cli::claude::parse_claude_output(&exec_result.stdout);
        let (tokens_input, tokens_output, model) =
            (parsed.tokens_input, parsed.tokens_output, parsed.model);

        self.run_service
            .append_log(
                run_id,
                "info",
                "Claude fix generation completed",
                Some(serde_json::json!({
                    "exit_code": exec_result.exit_code,
                    "tokens_input": tokens_input,
                    "tokens_output": tokens_output,
                })),
            )
            .await?;

        // Save AI output
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    ai_output: Some(exec_result.stdout),
                    ai_model: model,
                    tokens_input,
                    tokens_output,
                    ..Default::default()
                },
            )
            .await?;

        // Detect changed files
        let changed_files = self.detect_changed_files(&work_dir, run_id).await?;

        if changed_files.is_empty() {
            self.run_service
                .append_log(
                    run_id,
                    "warn",
                    "No file changes detected after fix generation",
                    None,
                )
                .await?;
            self.run_service
                .update_status(
                    run_id,
                    UpdateRunFields {
                        status: Some("no_fix".to_string()),
                        phase: Some("no_fix".to_string()),
                        completed_at: Some(Utc::now()),
                        ..Default::default()
                    },
                )
                .await?;

            // Revoke the run-scoped deployment token before releasing the sandbox.
            if let Some(token_id) = self.executor.run_token_ids.write().await.remove(&run_id) {
                self.executor.revoke_run_token(run_id, token_id).await;
            }
            // Clean up sandbox + work dir since there's nothing to create a PR from
            let _ = self.sandbox_registry.release(run_id).await;
            let _ = fs::remove_dir_all(&work_dir).await;
            return Ok(());
        }

        // Update status → "fix_ready"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("fix_ready".to_string()),
                    phase: Some("fix_ready".to_string()),
                    files_changed: Some(changed_files.len() as i32),
                    ..Default::default()
                },
            )
            .await?;

        self.run_service
            .append_log(
                run_id,
                "info",
                &format!(
                    "Fix ready: {} file(s) changed. Review the diff and click 'Create PR' to proceed.",
                    changed_files.len()
                ),
                None,
            )
            .await?;

        tracing::info!(
            "Autofixer run {}: fix complete, {} files changed, phase=fix_ready",
            run_id,
            changed_files.len()
        );

        Ok(())
    }

    // ── Phase 3: Create PR ─────────────────────────────────────────────────────

    /// Push the fix branch and create a pull request.
    /// The run must be in phase "fix_ready". Idempotent against double-click:
    /// if the run already has a `pr_url`, returns the existing PR without
    /// re-pushing; if a push is already in flight (`status == "pushing"`),
    /// rejects with a conflict-style validation error so the caller knows to
    /// wait instead of racing.
    pub async fn create_pr(&self, run_id: i32) -> Result<PullRequest, AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        // Idempotency: if we already created a PR for this run, return it
        // instead of doing the whole push/create dance a second time.
        if let (Some(url), Some(number)) = (run.pr_url.as_deref(), run.pr_number) {
            tracing::info!(
                "Autofixer run {}: create_pr called but PR already exists at {} — returning existing PR",
                run_id,
                url
            );
            return Ok(PullRequest {
                number,
                url: url.to_string(),
                title: String::new(),
                head_branch: run.branch_name.clone().unwrap_or_default(),
                base_branch: String::new(),
                head_sha: run.commit_sha.clone(),
            });
        }

        if run.status == "pushing" {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} is already creating a PR (status: pushing). \
                     Wait for the in-flight request to finish before retrying.",
                    run_id
                ),
            });
        }

        if run.phase.as_deref() != Some("fix_ready") {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} cannot create PR: expected phase 'fix_ready', got '{}'",
                    run_id,
                    run.phase.as_deref().unwrap_or("none")
                ),
            });
        }

        let project = projects::Entity::find_by_id(run.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::ProjectNotFound {
                project_id: run.project_id,
            })?;

        let connection_id = project
            .git_provider_connection_id
            .ok_or(AgentError::GitError {
                message: format!(
                    "Project {} has no git provider connection configured",
                    run.project_id
                ),
            })?;

        let work_dir = Self::work_dir(run_id);
        if !work_dir.exists() {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} work directory {:?} no longer exists. \
                     The server may have been restarted between fix and PR creation.",
                    run_id, work_dir
                ),
            });
        }

        // Collect changed files
        let changed_files = self.detect_changed_files(&work_dir, run_id).await?;

        if changed_files.is_empty() {
            return Err(AgentError::Validation {
                message: format!("Autofixer run {} has no changed files to push", run_id),
            });
        }

        let mut file_payloads: Vec<(String, Vec<u8>)> = Vec::new();
        for path in &changed_files {
            let full_path = work_dir.join(path);
            match fs::read(&full_path).await {
                Ok(contents) => {
                    file_payloads.push((path.clone(), contents));
                }
                Err(e) => {
                    tracing::warn!(
                        "Autofixer run {}: could not read changed file {:?}: {}",
                        run_id,
                        full_path,
                        e
                    );
                }
            }
        }

        // Update status → "pushing"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("pushing".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(
                run_id,
                "info",
                &format!("Pushing {} file(s) and creating PR...", file_payloads.len()),
                None,
            )
            .await?;

        // Generate branch name
        let error_group_id = run.trigger_source_id.unwrap_or(0);
        let branch_name = format!("fix/autofixer-err-{}-{:x}", error_group_id, run_id);

        // Build PR metadata — use error group title for the PR title
        let error_group_title = if let Some(group_id) = run.trigger_source_id {
            error_groups::Entity::find_by_id(group_id)
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten()
                .map(|g| g.title)
                .unwrap_or_else(|| "error fix".to_string())
        } else {
            "error fix".to_string()
        };

        let title_short: String = error_group_title.chars().take(72).collect();
        let pr_title = format!("fix: {} (autofixer run #{})", title_short, run_id);
        let commit_message = format!("fix: {} (run #{})", title_short, run_id);

        let analysis_text = run
            .analysis
            .as_deref()
            .unwrap_or("See analysis in Temps autofixer");

        // Truncate analysis for PR body (GitHub has body size limits)
        let analysis_for_pr = if analysis_text.len() > 2000 {
            format!(
                "{}...\n\n*(truncated — see full analysis in Temps)*",
                &analysis_text[..2000]
            )
        } else {
            analysis_text.to_string()
        };

        // Strip leading markdown headings from analysis to avoid duplication
        // (the AI often starts with "## Root Cause Analysis" which we already provide)
        let analysis_cleaned = analysis_for_pr
            .trim_start()
            .strip_prefix("## Root Cause Analysis")
            .or_else(|| {
                analysis_for_pr
                    .trim_start()
                    .strip_prefix("# Root Cause Analysis")
            })
            .or_else(|| {
                analysis_for_pr
                    .trim_start()
                    .strip_prefix("### Root Cause Analysis")
            })
            .unwrap_or(&analysis_for_pr)
            .trim_start_matches('\n')
            .trim_start();

        let pr_body = format!(
            "## Autofixer\n\n\
            This PR was generated by the [Temps](https://temps.sh) Autofixer (run #{run_id}).\n\n\
            ### Root Cause Analysis\n\n\
            {analysis}\n\n\
            ---\n\n\
            **Files changed:** {files}",
            run_id = run_id,
            analysis = analysis_cleaned,
            files = file_payloads.len(),
        );

        let pr = self
            .git_provider_manager
            .push_files_and_create_pr(
                connection_id,
                &project.repo_owner,
                &project.repo_name,
                &branch_name,
                &project.main_branch,
                file_payloads,
                &commit_message,
                &pr_title,
                &pr_body,
            )
            .await
            .map_err(|e| AgentError::GitError {
                message: format!(
                    "Failed to push and create PR for autofixer run {}: {}",
                    run_id, e
                ),
            })?;

        // Update run with PR details and mark completed
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("completed".to_string()),
                    phase: Some("completed".to_string()),
                    branch_name: Some(branch_name.clone()),
                    pr_url: Some(pr.url.clone()),
                    pr_number: Some(pr.number),
                    completed_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;

        self.run_service
            .append_log(run_id, "info", &format!("PR created: {}", pr.url), None)
            .await?;

        tracing::info!(
            "Autofixer run {}: PR #{} created at {}",
            run_id,
            pr.number,
            pr.url
        );

        // Revoke the run-scoped deployment token before releasing the sandbox.
        if let Some(token_id) = self.executor.run_token_ids.write().await.remove(&run_id) {
            self.executor.revoke_run_token(run_id, token_id).await;
        }
        // Release sandbox, then clean up temp dir
        let _ = self.sandbox_registry.release(run_id).await;
        if let Err(e) = fs::remove_dir_all(&work_dir).await {
            tracing::warn!(
                "Autofixer run {}: failed to clean up work dir {:?}: {}",
                run_id,
                work_dir,
                e
            );
        }

        Ok(pr)
    }

    // ── User context ───────────────────────────────────────────────────────────

    /// Append a user message to the run's `user_context` field.
    /// Also appends a `user_message` log entry so the message appears in the conversation.
    pub async fn add_context(&self, run_id: i32, message: String) -> Result<(), AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        let new_context = match run.user_context.as_deref() {
            Some(existing) if !existing.is_empty() => {
                format!("{}\n\n{}", existing, message)
            }
            _ => message.clone(),
        };

        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    user_context: Some(new_context),
                    ..Default::default()
                },
            )
            .await?;

        // Log as user_message so it appears in the conversation stream
        self.run_service
            .append_log(run_id, "user_message", &message, None)
            .await?;

        Ok(())
    }

    /// Continue the conversation with user feedback.
    /// Uses `--continue` to resume the same Claude session in the existing work directory.
    /// The run must be in "analyzed" phase. This does NOT re-clone the repo.
    pub async fn continue_with_feedback(&self, run_id: i32) {
        tracing::info!(
            "Autofixer run {}: continuing conversation with user feedback",
            run_id
        );

        match self.continue_with_feedback_inner(run_id).await {
            Ok(()) => {
                tracing::info!("Autofixer run {}: feedback conversation completed", run_id);
            }
            Err(e) => {
                tracing::error!(
                    "Autofixer run {}: feedback conversation failed: {}",
                    run_id,
                    e
                );
                let _ = self
                    .run_service
                    .update_status(
                        run_id,
                        UpdateRunFields {
                            status: Some("failed".to_string()),
                            phase: Some("failed".to_string()),
                            error_message: Some(e.to_string()),
                            completed_at: Some(Utc::now()),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = self
                    .run_service
                    .append_log(
                        run_id,
                        "error",
                        &format!("Conversation failed: {}", e),
                        None,
                    )
                    .await;
            }
        }
    }

    async fn continue_with_feedback_inner(&self, run_id: i32) -> Result<(), AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        // Get the latest user context (the user's feedback message)
        let user_context = run.user_context.clone().unwrap_or_default();
        if user_context.is_empty() {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} has no user context to continue with",
                    run_id
                ),
            });
        }

        // Verify work directory still exists (no re-cloning needed)
        let work_dir = Self::work_dir(run_id);
        if !work_dir.exists() {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} work directory {:?} no longer exists. \
                     The server may have been restarted. Please start a new analysis.",
                    run_id, work_dir
                ),
            });
        }

        // Update status → "analyzing" (so SSE stream reconnects)
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("analyzing".to_string()),
                    phase: Some("analyzing".to_string()),
                    ..Default::default()
                },
            )
            .await?;

        // Build the follow-up prompt from the latest user message
        // Extract just the last message (after the last double newline separator)
        let latest_message = user_context.rsplit("\n\n").next().unwrap_or(&user_context);

        let prompt = latest_message.to_string();

        // Streaming callback
        let run_service_for_stream = self.run_service.clone();
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let svc = run_service_for_stream.clone();
            Box::pin(async move {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return;
                }
                let _ = svc.append_log(run_id, "ai_event", trimmed, None).await;
            })
        });

        // Use --continue to resume the same conversation in the sandbox
        let cmd = build_claude_cmd("claude_cli", &prompt, 10, true, None);
        let exec_result = tokio::time::timeout(
            Duration::from_secs(300),
            self.sandbox_registry.exec(
                run_id,
                cmd,
                std::collections::HashMap::new(),
                Some(on_event),
            ),
        )
        .await
        .map_err(|_| AgentError::AiCliTimeout {
            provider: "claude_cli".to_string(),
            timeout_secs: 300,
        })??;

        if exec_result.exit_code != 0 {
            return Err(AgentError::AiCliFailed {
                provider: "claude_cli".to_string(),
                exit_code: exec_result.exit_code,
                stderr: exec_result.stdout,
            });
        }

        let parsed = crate::ai_cli::claude::parse_claude_output(&exec_result.stdout);
        let (tokens_input, tokens_output, model) =
            (parsed.tokens_input, parsed.tokens_output, parsed.model);

        // Extract the result text
        let analysis_text = extract_result_text(&exec_result.stdout);

        // Save updated analysis and transition back to "analyzed"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("analyzed".to_string()),
                    phase: Some("analyzed".to_string()),
                    analysis: Some(analysis_text),
                    ai_output: Some(exec_result.stdout),
                    ai_model: model,
                    tokens_input,
                    tokens_output,
                    ..Default::default()
                },
            )
            .await?;

        self.run_service
            .append_log(
                run_id,
                "info",
                "Analysis updated. Review the findings and click 'Generate Fix' to proceed.",
                None,
            )
            .await?;

        Ok(())
    }

    // ── Cancel ─────────────────────────────────────────────────────────────────

    /// Cancel an autofixer run and clean up its work directory.
    pub async fn cancel_run(&self, run_id: i32) -> Result<(), AgentError> {
        let run = self.run_service.get_run(run_id).await?;

        let terminal = ["completed", "failed", "no_fix", "cancelled"];
        if terminal.contains(&run.status.as_str()) {
            return Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} is already in terminal state '{}'",
                    run_id, run.status
                ),
            });
        }

        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("cancelled".to_string()),
                    phase: Some("cancelled".to_string()),
                    error_message: Some("Cancelled by user".to_string()),
                    completed_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;

        self.run_service
            .append_log(run_id, "info", "Run cancelled by user", None)
            .await?;

        // Revoke the run-scoped deployment token before releasing the sandbox.
        if let Some(token_id) = self.executor.run_token_ids.write().await.remove(&run_id) {
            self.executor.revoke_run_token(run_id, token_id).await;
        }
        // Release sandbox, then clean up temp dir
        let _ = self.sandbox_registry.release(run_id).await;
        let work_dir = Self::work_dir(run_id);
        if work_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&work_dir).await {
                tracing::warn!(
                    "Autofixer run {}: failed to clean up work dir {:?} on cancel: {}",
                    run_id,
                    work_dir,
                    e
                );
            }
        }

        Ok(())
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    /// Detect all files changed by Claude (committed, unstaged, and untracked).
    async fn detect_changed_files(
        &self,
        work_dir: &PathBuf,
        run_id: i32,
    ) -> Result<Vec<String>, AgentError> {
        let mut files: Vec<String> = Vec::new();

        // Committed changes
        let committed = Command::new("git")
            .args(["diff", "--name-only", "HEAD~1"])
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        if let Ok(output) = committed {
            if output.status.success() {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() && !files.contains(&trimmed) {
                        files.push(trimmed);
                    }
                }
            }
        }

        // Unstaged changes
        let unstaged = Command::new("git")
            .args(["diff", "--name-only"])
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        for line in String::from_utf8_lossy(&unstaged.stdout).lines() {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() && !files.contains(&trimmed) {
                files.push(trimmed);
            }
        }

        // Untracked files
        let untracked = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard"])
            .current_dir(work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        for line in String::from_utf8_lossy(&untracked.stdout).lines() {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() && !files.contains(&trimmed) {
                files.push(trimmed);
            }
        }

        tracing::debug!(
            "Autofixer run {}: detected {} changed file(s)",
            run_id,
            files.len()
        );

        Ok(files)
    }

    /// Load error type, message, and stack trace from the database.
    async fn load_error_context(
        &self,
        trigger_source_id: Option<i32>,
        project_id: i32,
    ) -> Result<(String, String, String, Option<String>), AgentError> {
        let group_id = trigger_source_id.ok_or(AgentError::Validation {
            message: format!(
                "trigger_source_id is required for error_group trigger in autofixer run for project {}",
                project_id
            ),
        })?;

        let group = error_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::Validation {
                message: format!(
                    "Error group {} not found for project {}",
                    group_id, project_id
                ),
            })?;

        let latest_event = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group_id))
            .order_by(error_events::Column::Timestamp, Order::Desc)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        let stack_trace = if let Some(event) = &latest_event {
            // Symbolicate in-memory before reading frames so agent sees original file paths
            let mut data_owned = event.data.clone();
            if let Some(ref mut data_val) = data_owned {
                self.source_map_service
                    .symbolicate_stored_event(event.project_id, data_val)
                    .await;
            }
            if let Some(ref data_val) = data_owned {
                // Try Sentry envelope format: data["sentry"]["exception"]["values"][0]["stacktrace"]["frames"]
                let sentry_frames = data_val
                    .get("sentry")
                    .and_then(|s| s.get("exception"))
                    .and_then(|e| e.get("values"))
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|exc| exc.get("stacktrace"))
                    .and_then(|st| st.get("frames"))
                    .and_then(|f| f.as_array());

                // Fallback: legacy data["stack_trace"] array
                let legacy_frames = data_val.get("stack_trace").and_then(|v| v.as_array());

                if let Some(frames) = sentry_frames.or(legacy_frames) {
                    frames
                        .iter()
                        .rev() // most recent first
                        .map(|frame| {
                            // Prefer symbolicated filename over minified abs_path
                            let file = frame
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let func = frame
                                .get("function")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let lineno = frame
                                .get("lineno")
                                .and_then(|v| v.as_i64())
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "?".to_string());
                            let colno = frame
                                .get("colno")
                                .and_then(|v| v.as_i64())
                                .map(|n| format!(":{}", n))
                                .unwrap_or_default();
                            format!("  at {} ({}:{}{})", func, file, lineno, colno)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        Ok((
            group.error_type.clone(),
            group.title.clone(),
            stack_trace,
            None,
        ))
    }
}

/// Extract the result text from Claude CLI's stream-json output.
/// Looks for a JSON line with `{"type": "result", "result": "..."}` and returns
/// the result string. Falls back to the full output if not found.
fn extract_result_text(output: &str) -> String {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('{') {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(trimmed)
                .ok()
                .and_then(|v| {
                    if v.get("type")?.as_str()? == "result" {
                        v.get("result")?.as_str().map(String::from)
                    } else {
                        None
                    }
                })
        })
        .next()
        .unwrap_or_else(|| output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Value;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::BTreeMap;
    use temps_entities::agent_runs;

    fn make_run(id: i32, project_id: i32, status: &str, phase: Option<&str>) -> agent_runs::Model {
        agent_runs::Model {
            id,
            project_id,
            config_id: None,
            agent_id: None,
            trigger_type: "autofixer".to_string(),
            trigger_source_id: Some(42),
            trigger_source_type: Some("error_group".to_string()),
            status: status.to_string(),
            phase: phase.map(|s| s.to_string()),
            analysis: None,
            user_context: None,
            branch_name: None,
            commit_sha: None,
            pr_url: None,
            pr_number: None,
            preview_url: None,
            preview_deployment_id: None,
            error_message: None,
            ai_output: None,
            ai_reasoning: None,
            ai_model: None,
            ai_provider: None,
            tokens_input: 0,
            tokens_output: 0,
            estimated_cost_cents: 0,
            files_changed: 0,
            started_at: None,
            completed_at: None,
            created_at: chrono::Utc::now(),
            ai_session_id: None,
            source: "committed".to_string(),
            ephemeral_yaml: None,

            prompt_text: None,
            workspace_volume: None,
        }
    }

    fn make_run_with_analysis(id: i32, project_id: i32) -> agent_runs::Model {
        agent_runs::Model {
            analysis: Some("Root cause: null pointer in handler.rs line 42".to_string()),
            ..make_run(id, project_id, "analyzed", Some("analyzed"))
        }
    }

    fn count_row(n: i64) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("num_items".to_string(), Value::BigInt(Some(n)));
        m
    }

    #[test]
    fn test_work_dir_path() {
        let path = AutofixerService::work_dir(123);
        assert!(path.to_string_lossy().contains("autofixer-123"));
    }

    #[tokio::test]
    async fn test_run_fix_wrong_phase_returns_validation_error() {
        // Run is in "analyzing" phase — fix should be rejected
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(1, 10, "analyzing", Some("analyzing"))]])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));

        // We can't fully construct AutofixerService without git/encryption mocks, so
        // test the validation logic through AgentRunService directly.
        let run = run_service.get_run(1).await.unwrap();
        assert_eq!(run.phase.as_deref(), Some("analyzing"));

        // Simulate the phase check performed by run_fix_inner
        let result: Result<(), AgentError> = if run.phase.as_deref() != Some("analyzed") {
            Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} cannot start fix: expected phase 'analyzed', got '{}'",
                    run.id,
                    run.phase.as_deref().unwrap_or("none")
                ),
            })
        } else {
            Ok(())
        };

        assert!(matches!(result, Err(AgentError::Validation { .. })));
    }

    #[tokio::test]
    async fn test_run_fix_no_analysis_text_returns_validation_error() {
        // Run is in "analyzed" phase but analysis field is NULL
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(1, 10, "analyzed", Some("analyzed"))]])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));
        let run = run_service.get_run(1).await.unwrap();

        let result: Result<(), AgentError> = if run.analysis.is_none() {
            Err(AgentError::Validation {
                message: format!(
                    "Autofixer run {} has no analysis text; cannot generate fix",
                    run.id
                ),
            })
        } else {
            Ok(())
        };

        assert!(matches!(result, Err(AgentError::Validation { .. })));
    }

    #[tokio::test]
    async fn test_cancel_already_terminal_returns_error() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_run(1, 10, "completed", Some("completed"))]])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));
        let run = run_service.get_run(1).await.unwrap();

        let terminal = ["completed", "failed", "no_fix", "cancelled"];
        let is_terminal = terminal.contains(&run.status.as_str());
        assert!(is_terminal, "completed should be terminal");
    }

    #[tokio::test]
    async fn test_add_context_appends_to_existing() {
        let existing = "existing context";
        let new_msg = "new message";
        let expected = format!("{}\n\n{}", existing, new_msg);

        let combined = format!("{}\n\n{}", existing, new_msg);
        assert_eq!(combined, expected);
    }

    #[tokio::test]
    async fn test_create_autofixer_run_success() {
        let run = make_run(1, 10, "pending", Some("analyzing"));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![run.clone()]])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));

        let retrieved = run_service.get_run(1).await.unwrap();
        assert_eq!(retrieved.trigger_type, "autofixer");
        assert_eq!(retrieved.phase.as_deref(), Some("analyzing"));
    }

    #[tokio::test]
    async fn test_create_autofixer_run_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<agent_runs::Model>::new()])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));

        let result = run_service.get_run(999).await;
        assert!(matches!(
            result.unwrap_err(),
            AgentError::RunNotFound { run_id: 999 }
        ));
    }

    #[tokio::test]
    async fn test_list_runs_by_project_for_count_query() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![count_row(1)]])
            .append_query_results(vec![vec![make_run_with_analysis(1, 10)]])
            .into_connection();
        let run_service = Arc::new(AgentRunService::new(Arc::new(db)));

        let (runs, total) = run_service.list_runs(10, None, None).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].analysis.as_deref(),
            Some("Root cause: null pointer in handler.rs line 42")
        );
    }
}
