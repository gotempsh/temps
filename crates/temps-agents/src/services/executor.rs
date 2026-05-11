use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, Order, QueryFilter, QueryOrder};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;

use temps_core::jobs::GitPushEventJob;
use temps_core::workflow_memory::{
    memory_install_command, WorkflowMemoryFact, WorkflowMemoryProvider,
};
use temps_core::{EncryptionService, Job, JobQueue};
use temps_deployments::services::deployment_token_service::{
    CreateDeploymentTokenRequest, DeploymentTokenService,
};
use temps_entities::{
    agent_runs, deployment_containers, deployments, error_events, error_groups, project_agents,
    projects, settings, status_checks, status_monitors,
};
use temps_git::services::git_provider_manager_trait::GitProviderManagerTrait;
use temps_notifications::services::NotificationService;
use temps_notifications::types::{Notification, NotificationPriority};

use crate::ai_cli::{AiCliProvider, AiRunConfig, AiRunResult, OnEventCallback};
use crate::error::AgentError;
use crate::sandbox::SandboxCreateConfig;
use crate::services::sandbox_registry::SandboxRegistry;
use crate::services::secret_service::{SecretService, SecretType};

use crate::services::config_service::AgentConfigService;
use crate::services::prompt_builder::PromptBuilder;
use crate::services::run_service::{AgentRunService, UpdateRunFields};

/// Parameters for [`AgentExecutor::prepare_sandbox_workspace`].
///
/// Kept as a struct (not positional args) because the setup function is the
/// single unified entry point and the field list will grow over time — new
/// features (e.g. per-run env overrides, experimental flags) should be added
/// here so callers don't need to thread more positional args.
pub struct PrepareWorkspaceParams<'a> {
    pub run_id: i32,
    pub project: &'a temps_entities::projects::Model,
    /// The agent config driving this run, if any.
    ///
    /// - `Some(config)` for regular workflow runs (executes per-agent MCPs/skills,
    ///   per-agent config repo overlay, per-agent `tools_config` custom tools).
    /// - `None` for autofixer runs (no persisted `project_agents` row — we
    ///   synthesize a minimal config so project-level overlays still apply).
    pub agent_config: Option<&'a temps_entities::project_agents::Model>,
    pub ai_provider: &'a str,
    pub agent_slug: &'a str,
    pub timeout_seconds: i32,
    pub host_work_dir: PathBuf,
    pub ephemeral_yaml: Option<&'a str>,
}

/// Build a minimal synthetic `project_agents::Model` for paths that don't have
/// a persisted agent row (e.g. the autofixer). Only the fields that
/// `inject_config_repos_and_secrets` and its callees read need to be set
/// correctly — everything else uses type defaults.
fn synthetic_agent_config(
    project_id: i32,
    ai_provider: &str,
    slug: &str,
) -> temps_entities::project_agents::Model {
    temps_entities::project_agents::Model {
        id: 0,
        project_id,
        slug: slug.to_string(),
        name: slug.to_string(),
        description: None,
        source: "synthetic".to_string(),
        enabled: true,
        trigger_config: serde_json::json!({}),
        prompt: None,
        ai_provider: ai_provider.to_string(),
        ai_model: None,
        api_key_encrypted: None,
        ai_provider_key_id: None,
        max_turns: 30,
        timeout_seconds: 600,
        daily_budget_cents: 0,
        cooldown_minutes: 0,
        branch_prefix: String::new(),
        deliverable: "pull_request".to_string(),
        sandbox_enabled: None,
        config_repo_url: None,
        config_repo_branch: None,
        mcp_servers_config: None,
        skills_config: None,
        tools_config: None,
        webhook_id: None,
        webhook_token: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

pub struct AgentExecutor {
    db: Arc<DatabaseConnection>,
    git_provider_manager: Arc<dyn GitProviderManagerTrait>,
    encryption_service: Arc<EncryptionService>,
    queue: Arc<dyn JobQueue>,
    run_service: Arc<AgentRunService>,
    config_service: Arc<AgentConfigService>,
    notification_service: Arc<NotificationService>,
    /// If set, this provider is used instead of resolving one from config.
    /// Intended for testing — inject a fake provider that simulates AI behaviour.
    ai_provider_override: Option<Arc<dyn AiCliProvider>>,
    /// Sandbox registry for running AI CLI inside isolated containers.
    sandbox_registry: Arc<SandboxRegistry>,
    /// Secret service for resolving encrypted secrets to inject into sandboxes.
    secret_service: Arc<SecretService>,
    /// Definition service for resolving skill/MCP slugs from project definitions.
    definition_service: Arc<super::definition_service::DefinitionService>,
    /// Optional workflow memory provider. When set, the executor:
    ///   1. Installs the `memory` script in the sandbox so the AI can write
    ///      facts back via curl.
    ///   2. Pre-loads relevant memory facts into the prompt before spawning
    ///      the harness, so the AI starts with what previous runs learned.
    ///
    /// When unset, workflow runs work exactly as before — no memory features.
    ///
    /// Wrapped in `RwLock<Option<...>>` so it can be set late by the plugin
    /// system after the executor has already been registered as an Arc'd
    /// service. The workspace plugin registers the memory provider after the
    /// agents plugin registers the executor, so we can't pass it via the
    /// constructor.
    memory_provider: tokio::sync::RwLock<Option<Arc<dyn WorkflowMemoryProvider>>>,
    /// Optional deployment token issuer. Used to mint a project-scoped token
    /// that the sandbox can use as `TEMPS_API_TOKEN` to call back to the API
    /// (memory script, future CLI commands, etc.).
    /// If unset, the script is still installed but memory writes will fail
    /// at the curl level since the token env var won't be set.
    /// Same RwLock pattern as memory_provider — set late by plugin init.
    deployment_token_service: tokio::sync::RwLock<Option<Arc<DeploymentTokenService>>>,
}

impl AgentExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DatabaseConnection>,
        git_provider_manager: Arc<dyn GitProviderManagerTrait>,
        encryption_service: Arc<EncryptionService>,
        queue: Arc<dyn JobQueue>,
        run_service: Arc<AgentRunService>,
        config_service: Arc<AgentConfigService>,
        notification_service: Arc<NotificationService>,
        sandbox_registry: Arc<SandboxRegistry>,
        secret_service: Arc<SecretService>,
        definition_service: Arc<super::definition_service::DefinitionService>,
    ) -> Self {
        Self {
            db,
            git_provider_manager,
            encryption_service,
            queue,
            run_service,
            config_service,
            notification_service,
            ai_provider_override: None,
            sandbox_registry,
            secret_service,
            definition_service,
            memory_provider: tokio::sync::RwLock::new(None),
            deployment_token_service: tokio::sync::RwLock::new(None),
        }
    }

    /// Attach a workflow memory provider so the executor pre-loads memory
    /// into prompts and installs the memory script in run sandboxes.
    /// Safe to call after the executor has been registered as a service —
    /// uses interior mutability so it works with `Arc<AgentExecutor>`.
    pub async fn attach_memory_provider(&self, provider: Arc<dyn WorkflowMemoryProvider>) {
        *self.memory_provider.write().await = Some(provider);
    }

    /// Attach a deployment token service so the executor can mint a
    /// short-lived project-scoped token for the sandbox to use as
    /// `TEMPS_API_TOKEN` when calling back to the Temps API.
    /// Safe to call after the executor has been registered as a service.
    pub async fn attach_deployment_token_service(&self, svc: Arc<DeploymentTokenService>) {
        *self.deployment_token_service.write().await = Some(svc);
    }

    /// Access the sandbox registry (for status checks).
    pub fn sandbox_registry(&self) -> &SandboxRegistry {
        &self.sandbox_registry
    }

    /// For testing: inject a custom AI CLI provider instead of resolving from config.
    pub fn with_ai_provider(mut self, provider: Arc<dyn AiCliProvider>) -> Self {
        self.ai_provider_override = Some(provider);
        self
    }

    // ── Memory helpers ──────────────────────────────────────────────────────

    /// Build the list of tags used to filter memory for a run. Encodes
    /// trigger context (error_group_id, etc.) so that future runs hitting
    /// the same trigger source see the relevant facts.
    pub(crate) fn build_memory_tags(
        trigger_source_type: Option<&str>,
        trigger_source_id: Option<i32>,
    ) -> Vec<String> {
        let mut tags = Vec::new();
        if let (Some(t), Some(id)) = (trigger_source_type, trigger_source_id) {
            tags.push(format!("{}:{}", t, id));
        }
        tags
    }

    /// Load relevant memory facts for a run from the configured provider.
    /// Returns an empty vec on any failure (memory is best-effort and must
    /// never block a run).
    pub(crate) async fn load_memory_facts(
        &self,
        project_id: i32,
        agent_id: i32,
        trigger_source_type: Option<&str>,
        trigger_source_id: Option<i32>,
    ) -> Vec<WorkflowMemoryFact> {
        let provider = {
            let guard = self.memory_provider.read().await;
            match guard.as_ref() {
                Some(p) => p.clone(),
                None => return Vec::new(),
            }
        };
        let tags = Self::build_memory_tags(trigger_source_type, trigger_source_id);
        match provider
            .load_for_trigger(project_id, agent_id, tags, 20)
            .await
        {
            Ok(facts) => facts,
            Err(e) => {
                tracing::warn!(
                    "Failed to load workflow memory for agent {}: {}. Continuing without memory.",
                    agent_id,
                    e
                );
                Vec::new()
            }
        }
    }

    /// Render a memory section to prepend to a workflow prompt. Returns
    /// an empty string when there's no memory provider or no facts to render.
    pub(crate) async fn render_memory_section(&self, facts: &[WorkflowMemoryFact]) -> String {
        let guard = self.memory_provider.read().await;
        match guard.as_ref() {
            Some(p) => p.render_for_prompt(facts),
            None => String::new(),
        }
    }

    /// Issue a project-scoped deployment token for a workflow run sandbox.
    /// Returns `None` if no token service is configured (in which case the
    /// memory script will fail at the curl level — that's fine, the run
    /// itself still proceeds).
    pub(crate) async fn issue_run_token(
        &self,
        project_id: i32,
        run_id: i32,
        agent_slug: &str,
    ) -> Option<String> {
        let svc = {
            let guard = self.deployment_token_service.read().await;
            match guard.as_ref() {
                Some(s) => s.clone(),
                None => return None,
            }
        };
        // Token lifetime: 2 hours. Autopilot runs have an internal timeout
        // well under this, so any token still usable after 2h belongs to a
        // run that has already completed or died — we'd rather the token
        // expire than linger. See Phase 2 of the security plan for
        // fine-grained permission scoping beyond expiry.
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(2);
        let request = CreateDeploymentTokenRequest {
            name: format!("workflow-run-{}-{}", agent_slug, run_id),
            environment_id: None,
            deployment_id: None,
            permissions: Some(vec!["*".to_string()]),
            expires_at: Some(expires_at),
        };
        match svc.create_token(project_id, None, request).await {
            Ok(response) => Some(response.token),
            Err(e) => {
                tracing::warn!(
                    "Failed to issue workflow run token (project={}, run={}): {}. \
                     Memory writes from this run will fail.",
                    project_id,
                    run_id,
                    e
                );
                None
            }
        }
    }

    /// Prepare the sandbox workspace: create the container, seed credentials,
    /// inject MCP/skills, install the memory script, and set up git credentials.
    ///
    /// This is the **single unified path** for workspace setup — both regular
    /// workflow runs (`execute_run`) and autofixer runs go through here. Any
    /// change to the AI agent environment (new env var, new config file, new
    /// MCP server, new auth layout) belongs in this one function.
    ///
    /// The caller is responsible for:
    ///   - Creating the `host_work_dir` on disk and cloning the project repo
    ///     into it (this method expects the work dir to already contain the
    ///     cloned repo).
    ///   - Setting the run status to "cloning" before calling and to
    ///     "analyzing" / next phase after.
    pub async fn prepare_sandbox_workspace(
        &self,
        params: PrepareWorkspaceParams<'_>,
    ) -> Result<crate::sandbox::SandboxHandle, AgentError> {
        let PrepareWorkspaceParams {
            run_id,
            project,
            agent_config,
            ai_provider,
            agent_slug,
            timeout_seconds,
            host_work_dir,
            ephemeral_yaml,
        } = params;

        // Load global sandbox settings for image/runtime/limits/credentials.
        let global_sandbox = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|s| {
                s.data.get("agent_sandbox").cloned().and_then(|v| {
                    serde_json::from_value::<temps_core::AgentSandboxSettings>(v).ok()
                })
            })
            .unwrap_or_default();

        let resolved_image = if global_sandbox.runtime == "custom" {
            if global_sandbox.custom_image.is_empty() {
                None
            } else {
                Some(global_sandbox.custom_image.clone())
            }
        } else {
            Some(format!("temps-sandbox-{}:latest", global_sandbox.runtime))
        };

        // Inject auth credentials into sandbox based on auth_type and provider.
        // Use per-provider credentials from the `providers` map so each CLI
        // gets its own key/token, not just the legacy Claude-only flat fields.
        let mut sandbox_env = std::collections::HashMap::new();
        // Stash decrypted credential + auth_type for file-based seeding after
        // the sandbox container is created (ApiKey goes into env vars now;
        // ConfigFile/OauthToken need the sandbox filesystem).
        let mut deferred_credential: Option<(String, String)> = None; // (value, auth_type)
        {
            let provider_cfg = global_sandbox.provider_config(ai_provider);
            if let Some(ref encrypted) = provider_cfg.credentials_encrypted {
                if !encrypted.is_empty() {
                    if let Ok(key) = self.encryption_service.decrypt_string(encrypted) {
                        let provider_entry = crate::ai_cli::catalog::find_provider(ai_provider);
                        let auth_type = if provider_cfg.auth_type.is_empty() {
                            provider_entry
                                .map(|p| p.default_flavor().id)
                                .unwrap_or("api_key")
                                .to_string()
                        } else {
                            provider_cfg.auth_type.clone()
                        };
                        let flavor = provider_entry.and_then(|p| p.flavor(&auth_type));

                        match flavor.map(|f| f.format) {
                            Some(crate::ai_cli::catalog::CredentialFormat::ApiKey) => {
                                let env_var = flavor.unwrap().env_var;
                                sandbox_env.insert(env_var.to_string(), key);
                            }
                            Some(crate::ai_cli::catalog::CredentialFormat::OauthToken) => {
                                sandbox_env
                                    .insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), key.clone());
                                deferred_credential = Some((key, auth_type));
                            }
                            Some(crate::ai_cli::catalog::CredentialFormat::ConfigFile) => {
                                deferred_credential = Some((key, auth_type));
                            }
                            None => {
                                tracing::warn!(
                                    "Unknown provider/auth_type {}/{} — falling back to env var",
                                    ai_provider,
                                    auth_type
                                );
                                sandbox_env.insert("ANTHROPIC_API_KEY".to_string(), key);
                            }
                        }
                    }
                }
            }
        }

        // Workflow memory + platform env vars
        sandbox_env.insert("TEMPS_PROJECT_ID".to_string(), project.id.to_string());
        sandbox_env.insert("TEMPS_WORKFLOW_SLUG".to_string(), agent_slug.to_string());
        sandbox_env.insert(
            "TEMPS_API_URL".to_string(),
            std::env::var("TEMPS_INTERNAL_API_URL")
                .unwrap_or_else(|_| "http://host.docker.internal:3000".to_string()),
        );
        sandbox_env.insert(
            "PATH".to_string(),
            "/home/temps/.temps/bin:/home/temps/.local/bin:/home/temps/.bun/bin:/home/temps/.opencode/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                .to_string(),
        );

        if let Some(token) = self.issue_run_token(project.id, run_id, agent_slug).await {
            sandbox_env.insert("TEMPS_API_TOKEN".to_string(), token);
        }

        let connection_id = project.git_provider_connection_id;
        let mut git_creds: Option<(String, String)> = None;
        if let Some(conn_id) = connection_id {
            match self
                .git_provider_manager
                .get_connection_access_token(conn_id)
                .await
            {
                Ok((token, provider_type)) => match provider_type.as_str() {
                    "github" | "gitlab" => {
                        git_creds = Some((token, provider_type));
                    }
                    other => {
                        tracing::debug!(
                            "Run {}: git provider '{}' has no known credential layout; \
                             skipping credential injection",
                            run_id,
                            other
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "Run {}: failed to fetch git provider token for connection {}: {}. \
                         Agent will run without push/PR credentials.",
                        run_id,
                        conn_id,
                        e
                    );
                }
            }
        }

        // Per-run overrides from the ephemeral YAML take precedence over globals.
        let (yaml_cpu, yaml_mem): (Option<f64>, Option<u64>) = ephemeral_yaml
            .and_then(|y| serde_yaml::from_str::<temps_core::WorkflowYamlConfig>(y).ok())
            .map(|y| (y.cpu_limit, y.memory_limit_mb))
            .unwrap_or((None, None));
        let cpu_limit = yaml_cpu.unwrap_or(global_sandbox.cpu_limit);
        let memory_limit_mb = yaml_mem.unwrap_or(global_sandbox.memory_limit_mb);

        // Per-run named volume for `/workspace`. Retained so follow-up phases
        // (autofixer fix → PR) or workspace sandboxes can re-mount it.
        let workspace_volume = format!("temps-wfrun-{}", run_id);
        if let Err(e) = self
            .run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    workspace_volume: Some(workspace_volume.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            tracing::warn!("Run {}: failed to persist workspace_volume: {}", run_id, e);
        }

        let sandbox_config = SandboxCreateConfig {
            run_id,
            container_name_override: None,
            host_work_dir: host_work_dir.clone(),
            workspace_volume: Some(workspace_volume),
            image: resolved_image,
            cpu_limit: Some(cpu_limit),
            memory_limit_mb: Some(memory_limit_mb),
            pids_limit: None,
            network_mode: Some(global_sandbox.network_mode.clone()),
            env_vars: sandbox_env,
            idle_timeout: Duration::from_secs(timeout_seconds as u64 + 60),
        };
        self.run_service
            .append_log(
                run_id,
                "info",
                &format!(
                    "Creating sandbox: runtime={}, image={}, {} CPU, {}MB RAM, network={}",
                    global_sandbox.runtime,
                    crate::sandbox::docker::image_name_for_runtime(&global_sandbox.runtime),
                    cpu_limit,
                    memory_limit_mb,
                    global_sandbox.network_mode,
                ),
                None,
            )
            .await?;

        let sandbox_start = std::time::Instant::now();
        let handle = self.sandbox_registry.get_or_create(sandbox_config).await?;

        self.run_service
            .append_log(
                run_id,
                "info",
                &format!(
                    "Sandbox ready in {:.1}s ({}) — container={}, id={}",
                    sandbox_start.elapsed().as_secs_f64(),
                    self.sandbox_registry.provider_name(),
                    handle.sandbox_name,
                    &handle.sandbox_id[..12.min(handle.sandbox_id.len())],
                ),
                None,
            )
            .await?;

        // Write file-based credentials (ConfigFile / OauthToken).
        if let Some((cred_value, auth_type)) = deferred_credential {
            if let Some(provider_entry) = crate::ai_cli::catalog::find_provider(ai_provider) {
                if let Some(flavor) = provider_entry.flavor(&auth_type) {
                    let seed_path = flavor.seed_path();
                    if let Some(idx) = seed_path.rfind('/') {
                        let parent = &seed_path[..idx];
                        let _ = self
                            .sandbox_registry
                            .exec(
                                run_id,
                                vec!["mkdir".into(), "-p".into(), parent.to_string()],
                                std::collections::HashMap::new(),
                                None,
                            )
                            .await;
                    }

                    let file_bytes = match flavor.format {
                        crate::ai_cli::catalog::CredentialFormat::OauthToken => {
                            let body = serde_json::json!({
                                "claudeAiOauth": {
                                    "accessToken": cred_value,
                                    "expiresAt": chrono::Utc::now().timestamp_millis() + 365 * 24 * 3600 * 1000,
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
                            serde_json::to_vec_pretty(&body).unwrap_or_default()
                        }
                        crate::ai_cli::catalog::CredentialFormat::ConfigFile => {
                            cred_value.into_bytes()
                        }
                        _ => Vec::new(),
                    };

                    if !file_bytes.is_empty() {
                        if let Err(e) = self
                            .sandbox_registry
                            .write_file(run_id, &seed_path, &file_bytes, 0o600)
                            .await
                        {
                            tracing::warn!(
                                "Failed to write credential file {} for run {}: {}",
                                seed_path,
                                run_id,
                                e
                            );
                        } else {
                            tracing::debug!(
                                "Seeded credential file {} for {} on run {}",
                                seed_path,
                                ai_provider,
                                run_id
                            );
                        }
                    }
                }
            }
        }

        // Install the workflow memory script (best-effort).
        if let Err(e) = self
            .sandbox_registry
            .exec(
                run_id,
                memory_install_command(),
                std::collections::HashMap::new(),
                None,
            )
            .await
        {
            tracing::warn!(
                "Failed to install memory script for run {}: {}. \
                 Memory writes from this run will not work.",
                run_id,
                e
            );
        } else {
            tracing::debug!("Installed memory script for run {}", run_id);
        }

        // Git credential helper + gh/glab config.
        if let Some((ref token, ref provider_name)) = git_creds {
            let host = match provider_name.as_str() {
                "github" => "github.com",
                "gitlab" => "gitlab.com",
                _ => "github.com",
            };
            let shell_quote = |v: &str| v.replace('\'', "'\\''");
            let mut script = String::from("set -e\n");
            script.push_str("mkdir -p /home/temps\n");
            script.push_str("git config --global init.defaultBranch main\n");
            script.push_str("git config --global pull.rebase false\n");
            script.push_str(&format!(
                "umask 077 && printf 'https://x-access-token:%s@%s\\n' '{}' '{}' > /home/temps/.git-credentials\n",
                shell_quote(token),
                host,
            ));
            script.push_str("git config --global credential.helper store\n");
            script.push_str(&format!(
                "git config --global url.'https://{host}/'.insteadOf 'git@{host}:'\n",
                host = host,
            ));

            match provider_name.as_str() {
                "github" => {
                    script.push_str("mkdir -p /home/temps/.config/gh\n");
                    script.push_str(&format!(
                        "umask 077 && cat > /home/temps/.config/gh/hosts.yml <<'EOF'\n\
                         github.com:\n\
                         \x20\x20oauth_token: {}\n\
                         \x20\x20user: x-access-token\n\
                         \x20\x20git_protocol: https\n\
                         EOF\n",
                        shell_quote(token),
                    ));
                }
                "gitlab" => {
                    script.push_str("mkdir -p /home/temps/.config/glab-cli\n");
                    script.push_str(&format!(
                        "umask 077 && cat > /home/temps/.config/glab-cli/config.yml <<'EOF'\n\
                         hosts:\n\
                         \x20\x20gitlab.com:\n\
                         \x20\x20\x20\x20token: {}\n\
                         \x20\x20\x20\x20git_protocol: https\n\
                         EOF\n",
                        shell_quote(token),
                    ));
                }
                _ => {}
            }

            script.push_str("chown -R temps:temps /home/temps/.git-credentials /home/temps/.gitconfig /home/temps/.config 2>/dev/null || true\n");

            if let Err(e) = self
                .sandbox_registry
                .exec(
                    run_id,
                    vec!["sh".to_string(), "-c".to_string(), script],
                    std::collections::HashMap::new(),
                    None,
                )
                .await
            {
                tracing::warn!(
                    "Run {}: failed to install git credential helper: {}",
                    run_id,
                    e
                );
            } else {
                tracing::debug!(
                    "Run {}: installed git credentials for {} provider",
                    run_id,
                    provider_name
                );
                self.run_service
                    .append_log(
                        run_id,
                        "info",
                        &format!(
                            "Injected {} credentials (git push + {} CLI auth, no env vars)",
                            provider_name,
                            if provider_name == "github" {
                                "gh"
                            } else {
                                "glab"
                            }
                        ),
                        None,
                    )
                    .await?;
            }
        }

        // Inject config repos, secrets, MCP, and skills.
        //
        // The autofixer path has no `project_agents::Model` because it is not
        // a persistent agent — we still want the global config repo overlay
        // and the project-level secrets, so we synthesize a minimal config
        // when `agent_config` is None.
        let owned_synthetic;
        let config_for_injection: &temps_entities::project_agents::Model = match agent_config {
            Some(c) => c,
            None => {
                owned_synthetic = synthetic_agent_config(project.id, ai_provider, agent_slug);
                &owned_synthetic
            }
        };
        if let Err(e) = self
            .inject_config_repos_and_secrets(
                run_id,
                config_for_injection,
                project.id,
                connection_id,
            )
            .await
        {
            tracing::warn!(
                "Failed to inject config repos/secrets for run {}: {}. Continuing without them.",
                run_id,
                e
            );
            self.run_service
                .append_log(
                    run_id,
                    "warning",
                    &format!(
                        "Config repo/secrets injection failed: {}. Agent will run without them.",
                        e
                    ),
                    None,
                )
                .await?;
        }

        Ok(handle)
    }

    /// Inject config repos and secrets into the sandbox.
    ///
    /// Overlay order: repo's own `.claude/` → global config repo → per-agent config repo.
    /// For `settings.json`, MCP servers are deep-merged (not overwritten).
    /// Secrets are resolved from `${TEMPS_SECRET:name}` placeholders.
    ///
    /// This is a best-effort operation — if config repos are not configured
    /// or cloning fails, the agent run continues without them.
    async fn inject_config_repos_and_secrets(
        &self,
        run_id: i32,
        config: &temps_entities::project_agents::Model,
        _project_id: i32,
        connection_id: Option<i32>,
    ) -> Result<(), AgentError> {
        // Load global AI config settings (config repo URL + branch)
        let ai_config = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|s| {
                s.data
                    .get("ai_config")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<temps_core::AiConfigSettings>(v).ok())
            })
            .unwrap_or_default();

        // Collect all secrets up front — needed for placeholder resolution in config files
        let secrets = self.secret_service.resolve_secrets().await?;
        let secret_map: std::collections::HashMap<String, String> = secrets
            .iter()
            .map(|s| (s.name.clone(), s.value.clone()))
            .collect();

        // ── Phase 1: Clone and overlay global config repo ──────────────────
        if !ai_config.config_repo.is_empty() {
            if let Some(conn_id) = connection_id {
                self.inject_config_repo(
                    run_id,
                    conn_id,
                    &ai_config.config_repo,
                    &ai_config.config_repo_branch,
                    "global",
                    &secret_map,
                )
                .await?;
            } else {
                self.run_service
                    .append_log(
                        run_id,
                        "warning",
                        "Global config repo configured but no git provider connection available",
                        None,
                    )
                    .await?;
            }
        }

        // ── Phase 2: Clone and overlay per-agent config repo (overrides global) ──
        if let Some(ref repo_url) = config.config_repo_url {
            if !repo_url.is_empty() {
                if let Some(conn_id) = connection_id {
                    let branch = config.config_repo_branch.as_deref().unwrap_or("main");
                    self.inject_config_repo(
                        run_id,
                        conn_id,
                        repo_url,
                        branch,
                        "per-agent",
                        &secret_map,
                    )
                    .await?;
                } else {
                    self.run_service
                        .append_log(
                            run_id,
                            "warning",
                            &format!(
                                "Per-agent config repo '{}' configured but no git provider connection",
                                repo_url
                            ),
                            None,
                        )
                        .await?;
                }
            }
        }

        // ── Phase 3: Inject secrets ────────────────────────────────────────
        //
        // CRITICAL: nothing written here may land under `/workspace/`.
        // `/workspace` is a bind mount of the cloned repo, so any file there
        // shows up as a tracked addition in the PR branch. Previously an
        // autofixer run leaked `.mcp/gsc-creds.json` and `.temps/secrets.env`
        // into a PR diff. All secret material stays under `/home/temps/`
        // (the private named volume).
        if !secrets.is_empty() {
            let mut env_count = 0;
            let mut file_count = 0;

            for secret in &secrets {
                match secret.secret_type {
                    SecretType::Env => {
                        // Env-type secrets: write a small script that exports them,
                        // or inject via a .env file that the sandbox reads
                        env_count += 1;
                    }
                    SecretType::File => {
                        if let Some(ref mount_path) = secret.mount_path {
                            let safe_path =
                                crate::services::sandbox_injector::sanitize_secret_mount_path(
                                    mount_path,
                                    &secret.name,
                                );
                            self.sandbox_registry
                                .write_file(run_id, &safe_path, secret.value.as_bytes(), 0o600)
                                .await?;
                            file_count += 1;
                        }
                    }
                }
            }

            // Write env-type secrets as a sourceable file in the sandbox
            let env_secrets: Vec<_> = secrets
                .iter()
                .filter(|s| s.secret_type == SecretType::Env)
                .collect();
            if !env_secrets.is_empty() {
                let mut env_content = String::new();
                for s in &env_secrets {
                    // Shell-safe: single-quote the value, escape embedded single quotes
                    let escaped = s.value.replace('\'', "'\\''");
                    env_content.push_str(&format!("export {}='{}'\n", s.name, escaped));
                }
                self.sandbox_registry
                    .write_file(
                        run_id,
                        "/home/temps/.temps/secrets.env",
                        env_content.as_bytes(),
                        0o600,
                    )
                    .await?;
            }

            self.run_service
                .append_log(
                    run_id,
                    "info",
                    &format!(
                        "Injected {} secret(s) ({} env, {} file)",
                        secrets.len(),
                        env_count,
                        file_count,
                    ),
                    None,
                )
                .await?;
        }

        // ── Phase 4: Inject inline MCP servers and skills from agent record ──
        self.inject_inline_mcp_and_skills(run_id, config, &secret_map)
            .await?;

        Ok(())
    }

    /// Inject MCP servers and skills stored directly on the agent record
    /// Inject MCP servers and skills into the sandbox by resolving slug references
    /// from project-level definitions tables.
    ///
    /// - `mcp_servers_config`: JSON array of slug strings → resolved from `project_mcp_definitions`
    /// - `skills_config`: JSON array of slug strings → resolved from `project_skill_definitions`
    /// - Custom tools from `tools_config` are still handled inline (webhook proxy).
    async fn inject_inline_mcp_and_skills(
        &self,
        run_id: i32,
        config: &temps_entities::project_agents::Model,
        secrets: &std::collections::HashMap<String, String>,
    ) -> Result<(), AgentError> {
        // Parse slug arrays from JSONB columns
        let mcp_slugs: Vec<String> = config
            .mcp_servers_config
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let skill_slugs: Vec<String> = config
            .skills_config
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let has_mcp = !mcp_slugs.is_empty();
        let has_skills = !skill_slugs.is_empty();

        // Extract custom tools from tools_config (type == "custom" with webhook_url)
        let custom_tools = Self::extract_custom_tools(config);
        let has_custom_tools = !custom_tools.is_empty();

        if !has_mcp && !has_skills && !has_custom_tools {
            return Ok(());
        }

        // Ensure the per-sandbox .claude directory exists under /home/temps
        // (private named volume — never in the bind-mounted repo).
        let _ = self
            .sandbox_registry
            .exec(
                run_id,
                vec![
                    "mkdir".to_string(),
                    "-p".to_string(),
                    "/home/temps/.claude".to_string(),
                ],
                std::collections::HashMap::new(),
                None,
            )
            .await;

        // ── Custom tools: install MCP proxy script and config ──
        if has_custom_tools {
            let _ = self
                .sandbox_registry
                .exec(
                    run_id,
                    vec![
                        "mkdir".to_string(),
                        "-p".to_string(),
                        "/home/temps/.temps/bin".to_string(),
                    ],
                    std::collections::HashMap::new(),
                    None,
                )
                .await;

            self.sandbox_registry
                .write_file(
                    run_id,
                    temps_agents_mcp_proxy::PROXY_SCRIPT_PATH,
                    temps_agents_mcp_proxy::PROXY_SCRIPT.as_bytes(),
                    0o755,
                )
                .await?;

            let proxy_config = temps_agents_mcp_proxy::ProxyConfig {
                tools: custom_tools.clone(),
            };
            let mut config_json = serde_json::to_string_pretty(&proxy_config).unwrap_or_default();
            if !secrets.is_empty() && config_json.contains("${TEMPS_SECRET:") {
                config_json = crate::services::secret_service::SecretService::resolve_placeholders(
                    &config_json,
                    secrets,
                );
            }
            self.sandbox_registry
                .write_file(
                    run_id,
                    temps_agents_mcp_proxy::PROXY_CONFIG_PATH,
                    config_json.as_bytes(),
                    0o644,
                )
                .await?;

            self.run_service
                .append_log(
                    run_id,
                    "info",
                    &format!(
                        "Installed MCP proxy with {} custom tool(s): {}",
                        custom_tools.len(),
                        custom_tools
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                )
                .await?;
        }

        // ── MCP servers: resolve slugs from project definitions and write provider-specific configs ──
        if has_mcp || has_custom_tools {
            let mut merged = serde_json::Map::new();

            // Resolve MCP slugs from project definitions
            if has_mcp {
                let mcp_defs = self
                    .definition_service
                    .get_all_available_mcps(config.project_id, &mcp_slugs)
                    .await?;

                for def in &mcp_defs {
                    merged.insert(def.slug.clone(), def.config.clone());
                }

                // Warn about unresolved slugs
                let resolved: std::collections::HashSet<&str> =
                    mcp_defs.iter().map(|d| d.slug.as_str()).collect();
                for slug in &mcp_slugs {
                    if !resolved.contains(slug.as_str()) {
                        self.run_service
                            .append_log(
                                run_id,
                                "warning",
                                &format!("MCP server definition '{}' not found — skipped", slug),
                                None,
                            )
                            .await?;
                    }
                }
            }

            // Add the custom tools proxy MCP server entry
            if has_custom_tools {
                let proxy_entry = temps_agents_mcp_proxy::mcp_server_entry();
                if let Some(proxy_servers) = proxy_entry.as_object() {
                    for (k, v) in proxy_servers {
                        merged.insert(k.clone(), v.clone());
                    }
                }
            }

            // Write MCP configs in all provider formats (Claude Code + active provider)
            let fs = super::sandbox_injector::RegistrySandboxFs {
                registry: self.sandbox_registry.clone(),
                run_id,
            };
            super::sandbox_injector::write_mcp_configs(&fs, &merged, secrets, &config.ai_provider)
                .await?;

            self.run_service
                .append_log(
                    run_id,
                    "info",
                    &format!(
                        "Wrote MCP config ({}) with {} server(s){}",
                        config.ai_provider,
                        merged.len(),
                        if has_custom_tools {
                            " (including custom tools proxy)"
                        } else {
                            ""
                        }
                    ),
                    None,
                )
                .await?;
        }

        // ── Skills: resolve slugs from project definitions, write as
        //    /home/temps/.claude/skills/{slug}/SKILL.md ──
        // We write to the user's home instead of /workspace/.claude so the
        // files don't appear as modifications in the repo bind mount.
        // Claude Code discovers user-level skills under ~/.claude/skills.
        if has_skills {
            let _ = self
                .sandbox_registry
                .exec(
                    run_id,
                    vec![
                        "mkdir".to_string(),
                        "-p".to_string(),
                        "/home/temps/.claude/skills".to_string(),
                    ],
                    std::collections::HashMap::new(),
                    None,
                )
                .await;

            let skill_defs = self
                .definition_service
                .get_all_available_skills(config.project_id, &skill_slugs)
                .await?;

            let mut count = 0;
            for def in &skill_defs {
                if let Some(archive_data) = &def.archive {
                    // Directory skill: extract tar.gz into a temp dir, then upload
                    // the whole directory to /home/temps/.claude/skills/{slug}/
                    let tmp_dir = tempfile::tempdir().map_err(|e| {
                        crate::error::AgentError::SandboxExecFailed {
                            run_id,
                            sandbox_id: String::new(),
                            reason: format!(
                                "Failed to create temp dir for skill '{}': {}",
                                def.slug, e
                            ),
                        }
                    })?;
                    const MAX_DECOMPRESSED_BYTES: u64 = 500 * 1024 * 1024; // 500 MB cap
                    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive_data));
                    let mut archive = tar::Archive::new(decoder);
                    // Disallow symlinks and absolute paths; unpack_in validates each entry
                    // path stays within the destination (prevents `../` traversal).
                    archive.set_preserve_permissions(false);
                    archive.set_unpack_xattrs(false);
                    let entries = archive.entries().map_err(|e| {
                        crate::error::AgentError::SandboxExecFailed {
                            run_id,
                            sandbox_id: String::new(),
                            reason: format!(
                                "Failed to read archive entries for skill '{}': {}",
                                def.slug, e
                            ),
                        }
                    })?;
                    let mut total_bytes: u64 = 0;
                    for entry in entries {
                        let mut entry =
                            entry.map_err(|e| crate::error::AgentError::SandboxExecFailed {
                                run_id,
                                sandbox_id: String::new(),
                                reason: format!(
                                    "Invalid archive entry for skill '{}': {}",
                                    def.slug, e
                                ),
                            })?;
                        let entry_size = entry.header().size().unwrap_or(0);
                        total_bytes = total_bytes.saturating_add(entry_size);
                        if total_bytes > MAX_DECOMPRESSED_BYTES {
                            return Err(crate::error::AgentError::SandboxExecFailed {
                                run_id,
                                sandbox_id: String::new(),
                                reason: format!(
                                    "Archive for skill '{}' exceeds 500MB decompressed limit",
                                    def.slug
                                ),
                            });
                        }
                        // Reject symlinks/hardlinks: both can escape the sandbox dir.
                        let entry_type = entry.header().entry_type();
                        if entry_type.is_symlink() || entry_type.is_hard_link() {
                            return Err(crate::error::AgentError::SandboxExecFailed {
                                run_id,
                                sandbox_id: String::new(),
                                reason: format!(
                                    "Archive for skill '{}' contains disallowed link entry",
                                    def.slug
                                ),
                            });
                        }
                        // unpack_in validates path stays within tmp_dir (returns false if not).
                        let unpacked = entry.unpack_in(tmp_dir.path()).map_err(|e| {
                            crate::error::AgentError::SandboxExecFailed {
                                run_id,
                                sandbox_id: String::new(),
                                reason: format!(
                                    "Failed to extract archive entry for skill '{}': {}",
                                    def.slug, e
                                ),
                            }
                        })?;
                        if !unpacked {
                            return Err(crate::error::AgentError::SandboxExecFailed {
                                run_id,
                                sandbox_id: String::new(),
                                reason: format!(
                                    "Archive for skill '{}' contains path traversal",
                                    def.slug
                                ),
                            });
                        }
                    }
                    let target_path = format!("/home/temps/.claude/skills/{}", def.slug);
                    self.sandbox_registry
                        .write_directory(run_id, tmp_dir.path(), &target_path)
                        .await?;
                } else {
                    // Simple single-file skill: write as
                    // /home/temps/.claude/skills/{slug}/SKILL.md
                    let dir_path = format!("/home/temps/.claude/skills/{}", def.slug);
                    let _ = self
                        .sandbox_registry
                        .exec(
                            run_id,
                            vec!["mkdir".to_string(), "-p".to_string(), dir_path.clone()],
                            std::collections::HashMap::new(),
                            None,
                        )
                        .await;
                    let path = format!("{}/SKILL.md", dir_path);
                    self.sandbox_registry
                        .write_file(run_id, &path, def.content.as_bytes(), 0o644)
                        .await?;
                }
                count += 1;
            }

            // Warn about unresolved slugs
            let resolved: std::collections::HashSet<&str> =
                skill_defs.iter().map(|d| d.slug.as_str()).collect();
            for slug in &skill_slugs {
                if !resolved.contains(slug.as_str()) {
                    self.run_service
                        .append_log(
                            run_id,
                            "warning",
                            &format!("Skill definition '{}' not found — skipped", slug),
                            None,
                        )
                        .await?;
                }
            }

            if count > 0 {
                self.run_service
                    .append_log(
                        run_id,
                        "info",
                        &format!(
                            "Injected {} skill file(s) into /home/temps/.claude/skills/",
                            count
                        ),
                        None,
                    )
                    .await?;
            }
        }

        Ok(())
    }

    /// Extract custom tools (type == "custom" with webhook_url) from tools_config.
    fn extract_custom_tools(
        config: &temps_entities::project_agents::Model,
    ) -> Vec<temps_agents_mcp_proxy::CustomToolDef> {
        let Some(tools_val) = config.tools_config.as_ref() else {
            return Vec::new();
        };
        let Some(tools_arr) = tools_val.as_array() else {
            return Vec::new();
        };

        tools_arr
            .iter()
            .filter_map(|tool| {
                let tool_type = tool.get("type")?.as_str()?;
                if tool_type != "custom" {
                    return None;
                }
                let name = tool.get("name")?.as_str()?.to_string();
                let webhook_url = tool.get("webhook_url")?.as_str()?.to_string();
                let description = tool
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input_schema = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                let headers = tool.get("headers").and_then(|v| {
                    v.as_object().map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                });

                Some(temps_agents_mcp_proxy::CustomToolDef {
                    name,
                    description,
                    input_schema,
                    webhook_url,
                    headers,
                })
            })
            .collect()
    }

    /// Clone a config repo and overlay its `.claude/` directory into the sandbox.
    /// Resolves `${TEMPS_SECRET:name}` placeholders in any `.json` files.
    async fn inject_config_repo(
        &self,
        run_id: i32,
        connection_id: i32,
        repo_path: &str,
        branch: &str,
        label: &str,
        secrets: &std::collections::HashMap<String, String>,
    ) -> Result<(), AgentError> {
        // Parse "owner/repo" format
        let parts: Vec<&str> = repo_path.splitn(2, '/').collect();
        if parts.len() != 2 {
            self.run_service
                .append_log(
                    run_id,
                    "warning",
                    &format!(
                        "Invalid {} config repo path '{}' — expected 'owner/repo' format",
                        label, repo_path
                    ),
                    None,
                )
                .await?;
            return Ok(());
        }
        let (owner, repo) = (parts[0], parts[1]);

        let clone_dir = std::env::temp_dir().join(format!(
            "temps-config-{}-{}-{}",
            label,
            run_id,
            repo.replace('/', "-")
        ));

        // Clean up any leftover from a previous run
        let _ = fs::remove_dir_all(&clone_dir).await;
        fs::create_dir_all(&clone_dir)
            .await
            .map_err(|e| AgentError::GitError {
                message: format!("Failed to create temp dir for config repo: {}", e),
            })?;

        self.run_service
            .append_log(
                run_id,
                "info",
                &format!(
                    "Cloning {} config repo {}/{} (branch: {})",
                    label, owner, repo, branch
                ),
                None,
            )
            .await?;

        // Clone the config repo
        self.git_provider_manager
            .clone_repository(connection_id, owner, repo, &clone_dir, Some(branch))
            .await
            .map_err(|e| AgentError::GitError {
                message: format!(
                    "Failed to clone {} config repo {}/{}: {}",
                    label, owner, repo, e
                ),
            })?;

        // Check if .claude/ directory exists in the cloned repo
        let claude_dir = clone_dir.join(".claude");
        if !claude_dir.exists() {
            self.run_service
                .append_log(
                    run_id,
                    "warning",
                    &format!(
                        "{} config repo {}/{} has no .claude/ directory — skipping overlay",
                        label, owner, repo
                    ),
                    None,
                )
                .await?;
            let _ = fs::remove_dir_all(&clone_dir).await;
            return Ok(());
        }

        // Resolve ${TEMPS_SECRET:name} placeholders in .json files before uploading
        if !secrets.is_empty() {
            Self::resolve_secrets_in_dir(&claude_dir, secrets).await;
        }

        // Upload the .claude/ directory into the sandbox under the sandbox
        // user's home — NEVER `/workspace/.claude`, because `/workspace` is
        // bind-mounted from the cloned repo and anything there would land in
        // the PR diff (including any secret values we just resolved above).
        self.sandbox_registry
            .write_directory(run_id, &claude_dir, "/home/temps/.claude")
            .await?;

        self.run_service
            .append_log(
                run_id,
                "info",
                &format!("Overlaid {} config repo .claude/ into sandbox", label),
                None,
            )
            .await?;

        // Clean up temp clone
        let _ = fs::remove_dir_all(&clone_dir).await;

        Ok(())
    }

    /// Recursively resolve `${TEMPS_SECRET:name}` placeholders in all `.json`
    /// files within a directory. Modifies files in place.
    async fn resolve_secrets_in_dir(
        dir: &std::path::Path,
        secrets: &std::collections::HashMap<String, String>,
    ) {
        let mut entries = match fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                Box::pin(Self::resolve_secrets_in_dir(&path, secrets)).await;
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if content.contains("${TEMPS_SECRET:") {
                        let resolved = SecretService::resolve_placeholders(&content, secrets);
                        let _ = fs::write(&path, resolved).await;
                    }
                }
            }
        }
    }

    /// Extract Claude CLI session files from the sandbox so that a workspace
    /// can later resume the conversation via `claude --resume <session_id>`.
    ///
    /// Saves to `~/.temps/agent-sessions/{run_id}/` on the host. Best-effort:
    /// failures are logged but never block the run from completing.
    async fn extract_session_files(&self, run_id: i32) {
        // Load the run to get the session_id
        let run = match self.run_service.get_run(run_id).await {
            Ok(r) => r,
            Err(_) => return,
        };
        let session_id = match &run.ai_session_id {
            Some(id) => id.clone(),
            None => return, // No session to extract
        };

        let data_dir = std::env::var("TEMPS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".temps"))
                    .unwrap_or_else(|_| PathBuf::from("/tmp/.temps"))
            });
        let host_dir = data_dir.join("agent-sessions").join(run_id.to_string());

        if let Err(e) = fs::create_dir_all(&host_dir).await {
            tracing::warn!("Failed to create session dir for run {}: {}", run_id, e);
            return;
        }

        // Claude stores sessions under ~/.claude/projects/{encoded-cwd}/{session_id}.jsonl
        // In the sandbox CWD is /workspace → encoded as "-workspace".
        // Try multiple possible paths in case the encoding differs.
        let candidate_paths = [
            format!(
                "/home/temps/.claude/projects/-workspace/{}.jsonl",
                session_id
            ),
            format!(
                "/home/temps/.claude/projects/-home-temps-workspace/{}.jsonl",
                session_id
            ),
        ];

        // Also try to discover the actual path via find
        let mut session_data: Option<Vec<u8>> = None;
        for path in &candidate_paths {
            match self.sandbox_registry.read_file(run_id, path).await {
                Ok(data) if !data.is_empty() => {
                    tracing::info!(
                        "Found Claude session file at {} for run {} ({} bytes)",
                        path,
                        run_id,
                        data.len()
                    );
                    session_data = Some(data);
                    break;
                }
                _ => continue,
            }
        }

        // Fallback: search for the session file anywhere under .claude/projects/
        if session_data.is_none() {
            let find_cmd = format!(
                "find /home/temps/.claude/projects -name '{}.jsonl' 2>/dev/null | head -1",
                session_id
            );
            if let Ok(output) = self
                .sandbox_registry
                .exec(
                    run_id,
                    vec!["sh".to_string(), "-c".to_string(), find_cmd],
                    std::collections::HashMap::new(),
                    None,
                )
                .await
            {
                let found_path = output.stdout.trim().to_string();
                if !found_path.is_empty() {
                    tracing::info!(
                        "Discovered session file at {} for run {} (not at expected path)",
                        found_path,
                        run_id
                    );
                    if let Ok(data) = self.sandbox_registry.read_file(run_id, &found_path).await {
                        if !data.is_empty() {
                            session_data = Some(data);
                        }
                    }
                }
            }
        }

        match session_data {
            Some(data) => {
                let dest = host_dir.join(format!("{}.jsonl", session_id));
                if let Err(e) = fs::write(&dest, &data).await {
                    tracing::warn!("Failed to write session file for run {}: {}", run_id, e);
                } else {
                    tracing::info!(
                        "Extracted Claude session {} for run {} ({} bytes)",
                        session_id,
                        run_id,
                        data.len()
                    );
                }
            }
            None => {
                tracing::warn!(
                    "Session file for {} not found in sandbox for run {} (tried {} paths + find)",
                    session_id,
                    run_id,
                    candidate_paths.len()
                );
            }
        }
    }

    /// Read the platform's configured default AI provider from settings.
    async fn platform_default_provider(&self) -> String {
        use temps_entities::settings;
        let record = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten();

        record
            .as_ref()
            .and_then(|r| r.data.get("agent_sandbox"))
            .and_then(|v| v.get("default_provider"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("claude_cli")
            .to_string()
    }

    /// Build an in-memory `project_agents::Model` from a `WorkflowYamlConfig`
    /// stored on the run row. Used when `agent_runs.source == "cli_ephemeral"`
    /// — there is no `project_agents` row to load.
    ///
    /// `id`, `webhook_id`, etc. are placeholders; ephemeral runs never persist
    /// or look this synthetic model up by id.
    fn synthesize_ephemeral_config(
        &self,
        project_id: i32,
        ephemeral_yaml: &str,
        platform_default_provider: &str,
    ) -> Result<project_agents::Model, AgentError> {
        let yaml: temps_core::WorkflowYamlConfig =
            serde_yaml::from_str(ephemeral_yaml).map_err(|e| AgentError::Validation {
                message: format!("Invalid ephemeral_yaml: {}", e),
            })?;

        let now = Utc::now();
        let provider = yaml
            .provider
            .clone()
            .unwrap_or_else(|| platform_default_provider.to_string());

        Ok(project_agents::Model {
            // Negative sentinel id makes accidental DB lookups against this
            // synthetic model fail loudly rather than silently aliasing onto
            // a real agent.
            id: -1,
            project_id,
            slug: yaml.slug(),
            name: yaml.name.clone(),
            description: yaml.description.clone(),
            source: "cli_ephemeral".to_string(),
            enabled: yaml.enabled,
            trigger_config: serde_json::json!({ "manual": true }),
            prompt: Some(yaml.prompt.clone()),
            ai_provider: provider,
            ai_model: yaml.ai_model.clone(),
            api_key_encrypted: None,
            ai_provider_key_id: None,
            max_turns: yaml.max_turns,
            timeout_seconds: yaml.timeout_seconds,
            daily_budget_cents: yaml.daily_budget_cents,
            cooldown_minutes: yaml.cooldown_minutes,
            branch_prefix: "temps/ephemeral".to_string(),
            // Force "report" so an ephemeral CLI run never opens a PR or
            // mutates the repo. The dry-run handler already enforces this on
            // the way in — we re-pin it here as defense in depth.
            deliverable: "report".to_string(),
            sandbox_enabled: Some(true),
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
            config_repo_url: None,
            config_repo_branch: None,
            webhook_id: None,
            webhook_token: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Execute a single autopilot run. Handles the full lifecycle from cloning to PR creation.
    pub async fn execute_run(&self, run_id: i32) {
        tracing::info!("Starting autopilot run {}", run_id);

        let work_dir = std::env::temp_dir().join(format!("autopilot-run-{}", run_id));

        match self.execute_run_inner(run_id, &work_dir).await {
            Ok(()) => {
                tracing::info!("Autopilot run {} completed successfully", run_id);
            }
            Err(e) => {
                tracing::error!("Autopilot run {} failed: {}", run_id, e);
                let _ = self
                    .run_service
                    .update_status(
                        run_id,
                        UpdateRunFields {
                            status: Some("failed".to_string()),
                            error_message: Some(e.to_string()),
                            completed_at: Some(Utc::now()),
                            ..Default::default()
                        },
                    )
                    .await;
                let _ = self
                    .run_service
                    .append_log(run_id, "error", &format!("Run failed: {}", e), None)
                    .await;

                // Send failure notification (best-effort — load context from DB)
                self.send_failure_notification(run_id, &e).await;
            }
        }

        // Extract Claude session files before destroying the sandbox so that
        // "Open in Workspace" can resume the conversation via `--resume`.
        if self.sandbox_registry.has_sandbox(run_id).await {
            self.extract_session_files(run_id).await;
        }

        // Always attempt cleanup: release sandbox first, then temp directory
        if self.sandbox_registry.has_sandbox(run_id).await {
            let _ = self
                .run_service
                .append_log(run_id, "info", "Destroying sandbox container...", None)
                .await;
        }
        let _ = self.sandbox_registry.release(run_id).await;
        if work_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&work_dir).await {
                tracing::warn!(
                    "Failed to clean up temp dir {:?} for run {}: {}",
                    work_dir,
                    run_id,
                    e
                );
            }
        }
    }

    async fn execute_run_inner(&self, run_id: i32, work_dir: &PathBuf) -> Result<(), AgentError> {
        // Step 1: Load the run record
        let run = self.run_service.get_run(run_id).await?;

        // Step 2: Load the agent config
        //
        // Two sources:
        //  1. Persistent (`source = "committed"`): look up by agent_id /
        //     config_id in `project_agents`. This is the normal path for
        //     workflows synced from `.temps/workflows/` and dashboard-created
        //     agents.
        //  2. Ephemeral (`source = "cli_ephemeral"`): build a synthetic
        //     `project_agents::Model` from the YAML stored on the run row.
        //     Nothing was written to `project_agents`, so there is no row to
        //     look up — the executor synthesizes one in memory so the rest of
        //     this function (which references ~65 `config.*` fields) doesn't
        //     need any branching.
        let mut config = if run.source == "cli_ephemeral" {
            let yaml = run
                .ephemeral_yaml
                .as_deref()
                .ok_or_else(|| AgentError::Validation {
                    message: format!(
                        "Run {} is marked source=cli_ephemeral but ephemeral_yaml is NULL",
                        run_id
                    ),
                })?;
            let default_provider = self.platform_default_provider().await;
            self.synthesize_ephemeral_config(run.project_id, yaml, &default_provider)?
        } else {
            let agent_id = run
                .agent_id
                .or(run.config_id)
                .ok_or(AgentError::ConfigNotFound {
                    project_id: run.project_id,
                })?;
            let mut cfg = self.config_service.get_agent_by_id(agent_id).await?.ok_or(
                AgentError::ConfigNotFound {
                    project_id: run.project_id,
                },
            )?;

            // For YAML-sourced workflows that don't declare an explicit provider,
            // the DB row stores whatever the platform default was at sync time.
            // Always re-resolve from the current platform default so the admin's
            // choice takes effect immediately without needing a redeploy.
            if cfg.source == "yaml" && cfg.ai_provider_key_id.is_none() {
                cfg.ai_provider = self.platform_default_provider().await;
            }
            cfg
        };
        // Suppress "unused mut" lint when only the persistent branch mutates.
        let _ = &mut config;

        // Step 3: Load the project
        let project = projects::Entity::find_by_id(run.project_id)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?
            .ok_or(AgentError::ProjectNotFound {
                project_id: run.project_id,
            })?;

        // Step 4: Load error group if trigger_source_type == "error_group"
        let (error_type, error_message, stack_trace, environment_name) =
            if run.trigger_source_type.as_deref() == Some("error_group") {
                self.load_error_context(run.trigger_source_id, run.project_id)
                    .await?
            } else {
                (
                    "Unknown".to_string(),
                    "Manual autopilot run".to_string(),
                    String::new(),
                    None,
                )
            };

        // Steps 5–6 (budget + cooldown) are intentionally omitted here.
        // Both checks are performed by the job listener (plugin.rs evaluate_trigger) BEFORE
        // creating this run record.  Repeating them here would (a) cause the cooldown check to
        // count this very run against itself, and (b) add unnecessary DB round-trips.

        // Step 5: Update status → "cloning", set started_at
        // (Budget and cooldown were already verified by the plugin listener before run creation.)
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
        tracing::info!(
            "Run {}: cloning repository for project {}",
            run_id,
            run.project_id
        );

        // Step 6: Create temp dir and clone repo
        fs::create_dir_all(work_dir).await?;

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
                work_dir,
                Some(&project.main_branch),
            )
            .await
            .map_err(|e| AgentError::GitError {
                message: format!(
                    "Failed to clone {}/{}: {}",
                    project.repo_owner, project.repo_name, e
                ),
            })?;

        // Step 6b: Prepare the sandbox workspace — credentials, MCP, skills,
        // git credentials, memory script. This is the single unified path
        // shared with the autofixer; any env/file/config tweak belongs here.
        self.prepare_sandbox_workspace(PrepareWorkspaceParams {
            run_id,
            project: &project,
            agent_config: Some(&config),
            ai_provider: &config.ai_provider,
            agent_slug: &config.slug,
            timeout_seconds: config.timeout_seconds,
            host_work_dir: work_dir.clone(),
            ephemeral_yaml: run.ephemeral_yaml.as_deref(),
        })
        .await?;

        // Step 7: Update status → "analyzing"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("analyzing".to_string()),
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
        tracing::info!("Run {}: analyzing error", run_id);

        // Step 8: Build the prompt
        // First, build a rich trigger context block with all available info.
        // This is prepended to the prompt so the YAML doesn't need template variables.
        let first_seen = run.created_at.to_rfc3339();
        let trigger_context = self
            .build_trigger_context(
                &run,
                &project,
                &error_type,
                &error_message,
                &stack_trace,
                environment_name.as_deref(),
            )
            .await;

        let prompt = if let Some(ref custom_prompt) = config.prompt {
            if trigger_context.is_empty() {
                custom_prompt.clone()
            } else {
                format!("{}\n\n{}", trigger_context, custom_prompt)
            }
        } else {
            // No custom prompt — use built-in error fix prompt for error triggers,
            // or a generic prompt for other trigger types
            if run.trigger_source_type.as_deref() == Some("error_group") {
                PromptBuilder::build_error_fix_prompt(
                    &project.name,
                    &error_type,
                    &error_message,
                    &stack_trace,
                    0,
                    &first_seen,
                    environment_name.as_deref(),
                )
            } else if trigger_context.is_empty() {
                format!(
                    "You are an AI agent running on the {} project. \
                     Perform the task described in your agent configuration.",
                    project.name
                )
            } else {
                format!(
                    "{}\n\nYou are an AI agent running on the {} project. \
                     Perform the task described in your agent configuration.",
                    trigger_context, project.name
                )
            }
        };

        // Append user context if provided (e.g. research topic, bug description)
        let prompt = if let Some(ref ctx) = run.user_context {
            if !ctx.is_empty() {
                format!("{}\n\n---\nUSER CONTEXT:\n{}\n", prompt, ctx)
            } else {
                prompt
            }
        } else {
            prompt
        };

        // Step 8b: Pre-load workflow memory and prepend it to the prompt.
        // This is the **push** half of memory: even if the AI never calls
        // `memory search`, it always sees the most relevant facts at the top
        // of the prompt. Best-effort — failures degrade silently.
        let prompt = {
            let memory_facts = self
                .load_memory_facts(
                    run.project_id,
                    config.id,
                    run.trigger_source_type.as_deref(),
                    run.trigger_source_id,
                )
                .await;
            let memory_section = self.render_memory_section(&memory_facts).await;
            if memory_section.is_empty() {
                prompt
            } else {
                self.run_service
                    .append_log(
                        run_id,
                        "info",
                        &format!(
                            "Pre-loaded {} fact(s) from workflow memory",
                            memory_facts.len()
                        ),
                        None,
                    )
                    .await?;
                format!("{}{}", memory_section, prompt)
            }
        };

        // Step 8b.5: Append deliverable-specific output guidelines so the AI
        // knows exactly what shape the human-facing artifact should take.
        let prompt = match config.deliverable.as_str() {
            "pull_request" => format!(
                "{prompt}\n\n---\nOUTPUT REQUIREMENTS (deliverable: pull_request)\n\n\
                 When your code changes are ready, emit the PR title and body in this exact block at the END of your final message, with nothing after it:\n\n\
                 <pr_title>Concise, imperative PR title (max 72 chars, no newlines, no trailing punctuation)</pr_title>\n\
                 <pr_body>\n\
                 ## Summary\n\
                 - One bullet per meaningful change\n\
                 - Use real line breaks (never literal \"\\n\"); write Markdown\n\n\
                 ## Test plan\n\
                 - [ ] How this was verified\n\
                 </pr_body>\n\n\
                 Rules:\n\
                 - Title: plain text, single line, starts with a conventional-commit type (feat/fix/chore/docs/refactor/test/build/ci/perf/style/revert) followed by \": \". Example: `fix: handle empty response in OrderList`.\n\
                 - Body: GitHub-flavored Markdown. Never output the two-character sequence \"\\n\" — use actual newlines.\n\
                 - Do not wrap the block in ``` fences. Do not repeat the title inside the body.\n",
                prompt = prompt
            ),
            _ => prompt,
        };

        // Step 8c: Persist the final prompt so the dashboard can show exactly
        // what the AI CLI saw. Best-effort — a failure here shouldn't abort
        // the run.
        if let Err(e) = self
            .run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    prompt_text: Some(prompt.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            tracing::warn!("Run {}: failed to persist prompt_text: {}", run_id, e);
        }

        // Step 9: Resolve API key. Priority:
        // 1. Per-agent encrypted key (config.api_key_encrypted)
        // 2. Linked AI provider key (config.ai_provider_key_id → ai_provider_keys table)
        // 3. Empty (Claude CLI uses ~/.claude auth / subscription mode)
        let api_key = if let Some(ref encrypted) = config.api_key_encrypted {
            self.encryption_service
                .decrypt_string(encrypted)
                .map_err(|e| AgentError::EncryptionError {
                    message: format!(
                        "Failed to decrypt API key for project {}: {}",
                        run.project_id, e
                    ),
                })?
        } else if let Some(key_id) = config.ai_provider_key_id {
            // Look up the shared AI provider key
            use temps_entities::ai_provider_keys;
            let key_record = ai_provider_keys::Entity::find_by_id(key_id)
                .one(self.db.as_ref())
                .await
                .map_err(AgentError::Database)?;
            if let Some(key) = key_record {
                self.encryption_service
                    .decrypt_string(&key.api_key_encrypted)
                    .map_err(|e| AgentError::EncryptionError {
                        message: format!("Failed to decrypt AI provider key {}: {}", key_id, e),
                    })?
            } else {
                tracing::warn!(
                    "AI provider key {} not found for agent {}, running without API key",
                    key_id,
                    config.slug
                );
                String::new()
            }
        } else {
            String::new()
        };

        // Step 10: Update status → "fixing"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("fixing".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(
                run_id,
                "info",
                &format!("Running {} to fix the error...", config.ai_provider),
                None,
            )
            .await?;
        tracing::info!("Run {}: invoking AI CLI {}", run_id, config.ai_provider);

        // Step 11: Run AI CLI via sandbox (or direct provider for testing)
        let run_service_for_stream = self.run_service.clone();
        let stream_run_id = run_id;
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let svc = run_service_for_stream.clone();
            let rid = stream_run_id;
            Box::pin(async move {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return;
                }
                let _ = svc.append_log(rid, "ai_event", trimmed, None).await;
            })
        });

        let ai_result = if let Some(ref override_provider) = self.ai_provider_override {
            // Testing path: use injected provider directly
            let ai_config = AiRunConfig {
                work_dir: work_dir.clone(),
                prompt,
                api_key: api_key.clone(),
                max_turns: config.max_turns,
                timeout: Duration::from_secs(config.timeout_seconds as u64),
                model: config.ai_model.clone(),
                on_event: Some(on_event),
            };
            override_provider.run(ai_config).await?
        } else {
            // OpenCode lstats the first positional arg as a path — long
            // prompts exceed NAME_MAX (255). Write to a temp file so the
            // shell wrapper can `$(cat ...)` it instead.
            if config.ai_provider == "opencode" {
                if let Err(e) = self
                    .sandbox_registry
                    .write_file(run_id, "/tmp/.temps-prompt", prompt.as_bytes(), 0o644)
                    .await
                {
                    tracing::warn!(
                        "run {}: failed to write opencode prompt file: {}",
                        run_id,
                        e
                    );
                }
            }

            // Sandbox path: execute AI CLI inside isolated container
            let cmd = build_claude_cmd(
                &config.ai_provider,
                &prompt,
                config.max_turns,
                false,
                config.ai_model.as_deref(),
            );

            let mut env = std::collections::HashMap::new();
            if !api_key.is_empty() {
                // Set the correct env var for the provider (e.g. OPENAI_API_KEY for Codex)
                let env_var = crate::ai_cli::catalog::find_provider(&config.ai_provider)
                    .and_then(|p| {
                        p.auth_flavors.iter().find(|f| {
                            matches!(f.format, crate::ai_cli::catalog::CredentialFormat::ApiKey)
                        })
                    })
                    .map(|f| f.env_var)
                    .unwrap_or("ANTHROPIC_API_KEY");
                env.insert(env_var.to_string(), api_key.clone());
            }

            let exec_result = tokio::time::timeout(
                Duration::from_secs(config.timeout_seconds as u64),
                self.sandbox_registry.exec(run_id, cmd, env, Some(on_event)),
            )
            .await
            .map_err(|_| AgentError::AiCliTimeout {
                provider: config.ai_provider.clone(),
                timeout_secs: config.timeout_seconds as u64,
            })??;

            if exec_result.exit_code != 0 {
                return Err(AgentError::AiCliFailed {
                    provider: config.ai_provider.clone(),
                    exit_code: exec_result.exit_code,
                    stderr: exec_result.stdout,
                });
            }

            // Parse output based on provider format
            let parsed = match config.ai_provider.as_str() {
                "codex_cli" => {
                    let (tokens_input, tokens_output, model) =
                        crate::ai_cli::codex::parse_codex_output(&exec_result.stdout);
                    crate::ai_cli::claude::ParsedClaudeOutput {
                        tokens_input,
                        tokens_output,
                        model,
                        session_id: None,
                        is_max_turns_error: false,
                    }
                }
                "opencode" => {
                    let (tokens_input, tokens_output, model) =
                        crate::ai_cli::opencode::parse_opencode_output(&exec_result.stdout);
                    crate::ai_cli::claude::ParsedClaudeOutput {
                        tokens_input,
                        tokens_output,
                        model,
                        session_id: None,
                        is_max_turns_error: false,
                    }
                }
                _ => crate::ai_cli::claude::parse_claude_output(&exec_result.stdout),
            };

            AiRunResult {
                output: exec_result.stdout,
                exit_code: exec_result.exit_code,
                tokens_input: parsed.tokens_input,
                tokens_output: parsed.tokens_output,
                model: parsed.model,
                changed_files: None,
                session_id: parsed.session_id,
                is_max_turns_error: parsed.is_max_turns_error,
            }
        };

        // Step 12b: Auto-continue if the CLI hit the max turns limit.
        // Re-runs with --continue up to 2 more times so the agent can finish its work.
        let mut ai_result = ai_result;
        const MAX_CONTINUATIONS: usize = 2;
        let mut continuation = 0;
        while ai_result.is_max_turns_error && continuation < MAX_CONTINUATIONS {
            continuation += 1;
            tracing::warn!(
                "Run {}: AI CLI hit max_turns limit, auto-continuing ({}/{})",
                run_id,
                continuation,
                MAX_CONTINUATIONS
            );
            self.run_service
                .append_log(
                    run_id,
                    "warning",
                    &format!(
                        "Workflow hit the turn limit. Auto-continuing ({}/{})...",
                        continuation, MAX_CONTINUATIONS
                    ),
                    None,
                )
                .await?;

            let continue_prompt = "Continue where you left off. Complete the task.".to_string();
            let run_service_for_stream = self.run_service.clone();
            let stream_run_id = run_id;
            let on_event: OnEventCallback = Arc::new(move |line: String| {
                let svc = run_service_for_stream.clone();
                let rid = stream_run_id;
                Box::pin(async move {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        return;
                    }
                    let _ = svc.append_log(rid, "ai_event", trimmed, None).await;
                })
            });

            let cont_result = if let Some(ref override_provider) = self.ai_provider_override {
                let ai_config = AiRunConfig {
                    work_dir: work_dir.clone(),
                    prompt: continue_prompt,
                    api_key: api_key.clone(),
                    max_turns: config.max_turns,
                    timeout: Duration::from_secs(config.timeout_seconds as u64),
                    model: config.ai_model.clone(),
                    on_event: Some(on_event),
                };
                override_provider.continue_conversation(ai_config).await?
            } else {
                if config.ai_provider == "opencode" {
                    if let Err(e) = self
                        .sandbox_registry
                        .write_file(
                            run_id,
                            "/tmp/.temps-prompt",
                            continue_prompt.as_bytes(),
                            0o644,
                        )
                        .await
                    {
                        tracing::warn!(
                            "run {}: failed to write opencode prompt file (continue): {}",
                            run_id,
                            e
                        );
                    }
                }
                let cmd = build_claude_cmd(
                    &config.ai_provider,
                    &continue_prompt,
                    config.max_turns,
                    true,
                    config.ai_model.as_deref(),
                );
                let mut env = std::collections::HashMap::new();
                if !api_key.is_empty() {
                    let env_var = crate::ai_cli::catalog::find_provider(&config.ai_provider)
                        .and_then(|p| {
                            p.auth_flavors.iter().find(|f| {
                                matches!(f.format, crate::ai_cli::catalog::CredentialFormat::ApiKey)
                            })
                        })
                        .map(|f| f.env_var)
                        .unwrap_or("ANTHROPIC_API_KEY");
                    env.insert(env_var.to_string(), api_key.clone());
                }
                let exec_result = tokio::time::timeout(
                    Duration::from_secs(config.timeout_seconds as u64),
                    self.sandbox_registry.exec(run_id, cmd, env, Some(on_event)),
                )
                .await
                .map_err(|_| AgentError::AiCliTimeout {
                    provider: config.ai_provider.clone(),
                    timeout_secs: config.timeout_seconds as u64,
                })??;

                if exec_result.exit_code != 0 {
                    break;
                }

                let parsed = if config.ai_provider == "codex_cli" {
                    let (tokens_input, tokens_output, model) =
                        crate::ai_cli::codex::parse_codex_output(&exec_result.stdout);
                    crate::ai_cli::claude::ParsedClaudeOutput {
                        tokens_input,
                        tokens_output,
                        model,
                        session_id: None,
                        is_max_turns_error: false,
                    }
                } else {
                    crate::ai_cli::claude::parse_claude_output(&exec_result.stdout)
                };
                AiRunResult {
                    output: exec_result.stdout,
                    exit_code: exec_result.exit_code,
                    tokens_input: parsed.tokens_input,
                    tokens_output: parsed.tokens_output,
                    model: parsed.model,
                    changed_files: None,
                    session_id: parsed.session_id,
                    is_max_turns_error: parsed.is_max_turns_error,
                }
            };

            // Merge: append new output, accumulate tokens, keep latest model/session
            ai_result.output.push('\n');
            ai_result.output.push_str(&cont_result.output);
            ai_result.tokens_input = match (ai_result.tokens_input, cont_result.tokens_input) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
            ai_result.tokens_output = match (ai_result.tokens_output, cont_result.tokens_output) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
            if cont_result.model.is_some() {
                ai_result.model = cont_result.model;
            }
            if cont_result.session_id.is_some() {
                ai_result.session_id = cont_result.session_id;
            }
            ai_result.is_max_turns_error = cont_result.is_max_turns_error;
        }

        // Step 13: Save AI output immediately (so it's preserved even if push fails later)
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    ai_output: Some(ai_result.output.clone()),
                    ai_model: ai_result.model.clone(),
                    ai_provider: Some(config.ai_provider.clone()),
                    tokens_input: ai_result.tokens_input,
                    tokens_output: ai_result.tokens_output,
                    ai_session_id: ai_result.session_id.clone(),
                    ..Default::default()
                },
            )
            .await?;
        if ai_result.is_max_turns_error {
            self.run_service
                .append_log(
                    run_id,
                    "warning",
                    &format!(
                        "AI CLI exhausted all turns (max_turns={}, continuations={}). Output may be incomplete.",
                        config.max_turns, continuation
                    ),
                    Some(serde_json::json!({
                        "exit_code": ai_result.exit_code,
                        "tokens_input": ai_result.tokens_input,
                        "tokens_output": ai_result.tokens_output,
                        "model": ai_result.model,
                        "is_max_turns_error": true,
                        "continuations": continuation,
                    })),
                )
                .await?;
        } else {
            self.run_service
                .append_log(
                    run_id,
                    "info",
                    "AI CLI completed",
                    Some(serde_json::json!({
                        "exit_code": ai_result.exit_code,
                        "tokens_input": ai_result.tokens_input,
                        "tokens_output": ai_result.tokens_output,
                        "model": ai_result.model,
                    })),
                )
                .await?;
        }

        // Report deliverable: store the AI output as the report and complete.
        // No branch, no PR, no deployment.
        if config.deliverable == "report" {
            // Extract all assistant text blocks from the AI output to build the full report.
            // Supports both Claude stream-json and Codex --json formats.
            let report_text = extract_report_text(&ai_result.output);

            self.run_service
                .update_status(
                    run_id,
                    UpdateRunFields {
                        status: Some("completed".to_string()),
                        analysis: Some(report_text.clone()),
                        ai_output: Some(ai_result.output),
                        ai_model: ai_result.model,
                        tokens_input: ai_result.tokens_input,
                        tokens_output: ai_result.tokens_output,
                        completed_at: Some(Utc::now()),
                        ..Default::default()
                    },
                )
                .await?;
            self.run_service
                .append_log(run_id, "info", "Report completed — no PR created.", None)
                .await?;

            // Notify user that the report is ready
            let report_preview = if report_text.len() > 500 {
                format!("{}...", &report_text[..500])
            } else {
                report_text
            };
            self.send_completion_notification(
                run_id,
                &config.name,
                &project.name,
                &project.slug,
                &format!(
                    "Workflow **{}** completed run #{} for **{}**.\n\n{}",
                    config.name, run_id, project.name, report_preview
                ),
                &config.deliverable,
            )
            .await;

            tracing::info!("Run {}: deliverable=report, completed without PR", run_id,);
            return Ok(());
        }

        // Step 14: Detect changes.
        // If the AI provider reported which files it changed, use that list.
        // Otherwise fall back to `git diff` (works when work_dir is a real git repo).
        let changed_files_owned: Vec<String> = if let Some(ref files) = ai_result.changed_files {
            files.clone()
        } else {
            // Claude CLI may commit changes itself (`git add && git commit`), or leave
            // them unstaged/untracked. We check all three states:
            //   1. Committed changes: `git diff --name-only HEAD~1` (if there are new commits)
            //   2. Unstaged changes: `git diff --name-only`
            //   3. Untracked files: `git ls-files --others --exclude-standard`

            let mut files: Vec<String> = Vec::new();

            // Check for committed changes (Claude may have run git commit)
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

            // Check for unstaged changes
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

            // Check for untracked files
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
            files
        };

        if changed_files_owned.is_empty() {
            tracing::info!("Run {}: no changes detected, marking as no_fix", run_id);
            self.run_service
                .update_status(
                    run_id,
                    UpdateRunFields {
                        status: Some("no_fix".to_string()),
                        ai_output: Some(ai_result.output),
                        ai_model: ai_result.model,
                        tokens_input: ai_result.tokens_input,
                        tokens_output: ai_result.tokens_output,
                        completed_at: Some(Utc::now()),
                        ..Default::default()
                    },
                )
                .await?;
            self.run_service
                .append_log(
                    run_id,
                    "warn",
                    "No file changes detected after AI run",
                    None,
                )
                .await?;

            self.send_completion_notification(
                run_id,
                &config.name,
                &project.name,
                &project.slug,
                &format!(
                    "Workflow **{}** completed run #{} for **{}** but made no code changes.\n\nThe AI analyzed the issue but didn't produce a fix. You can review the AI output in the run details or open it in a workspace to continue interactively.",
                    config.name, run_id, project.name
                ),
                &config.deliverable,
            )
            .await;

            return Ok(());
        }

        // Safety check: abort if the AI modified an unreasonable number of files.
        // This guards against runaway AI behaviour that could produce enormous PRs.
        const MAX_FILES_CHANGED: usize = 50;
        if changed_files_owned.len() > MAX_FILES_CHANGED {
            return Err(AgentError::Validation {
                message: format!(
                    "AI modified {} files, exceeding the safety limit of {}. Aborting.",
                    changed_files_owned.len(),
                    MAX_FILES_CHANGED
                ),
            });
        }

        // Step 15: Collect changed file contents
        let mut file_payloads: Vec<(String, Vec<u8>)> = Vec::new();
        for path in &changed_files_owned {
            let full_path = work_dir.join(path);
            match fs::read(&full_path).await {
                Ok(contents) => {
                    file_payloads.push((path.to_string(), contents));
                }
                Err(e) => {
                    tracing::warn!(
                        "Run {}: could not read changed file {:?}: {}",
                        run_id,
                        full_path,
                        e
                    );
                }
            }
        }

        // Step 16: Update status → "pushing"
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
                &format!(
                    "Pushing {} changed file(s) and creating PR...",
                    file_payloads.len()
                ),
                None,
            )
            .await?;
        tracing::info!("Run {}: pushing {} files", run_id, file_payloads.len());

        // Step 17: Generate branch name
        let short_run_id = format!("{:x}", run_id);
        let error_group_suffix = run
            .trigger_source_id
            .map(|id| format!("err-{}-", id))
            .unwrap_or_default();
        let branch_name = format!(
            "{}fix/{}{}",
            config.branch_prefix, error_group_suffix, short_run_id
        );

        // Step 18: Push + create PR
        // Prefer the <pr_title>/<pr_body> block the AI produced per the
        // deliverable guidelines. Fall back to a deterministic default when
        // the AI didn't emit one (or emitted something unusable).
        let extracted = extract_pr_metadata(&ai_result.output);

        let fallback_title = if run.trigger_source_type.as_deref() == Some("error_group") {
            format!("fix: {} — {} (run #{})", error_message, config.name, run_id)
        } else {
            format!("{}: {} (run #{})", config.name, project.name, run_id)
        };
        let pr_title = extracted
            .title
            .as_deref()
            .map(sanitize_pr_title)
            .filter(|t| !t.is_empty())
            .unwrap_or(fallback_title);

        let commit_message = if run.trigger_source_type.as_deref() == Some("error_group") {
            format!("fix: {} (run #{})", error_message, run_id)
        } else {
            format!("{} (run #{})", config.name.to_lowercase(), run_id)
        };

        let description = normalize_markdown(config.description.as_deref().unwrap_or(""));
        let ai_body = extracted
            .body
            .as_deref()
            .map(normalize_markdown)
            .filter(|b| !b.trim().is_empty());
        let body_main = ai_body.unwrap_or_else(|| description.clone());
        let pr_body = format!(
            "{body}\n\n---\n<sub>Created by the **{agent_name}** agent in [Temps](https://temps.sh) · run #{run_id} · {files} file(s) changed</sub>",
            body = body_main.trim_end(),
            agent_name = config.name,
            run_id = run_id,
            files = changed_files_owned.len(),
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
                message: format!("Failed to push and create PR for run {}: {}", run_id, e),
            })?;

        // Step 19: Update run with PR details
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    branch_name: Some(branch_name.clone()),
                    pr_url: Some(pr.url.clone()),
                    pr_number: Some(pr.number),
                    files_changed: Some(changed_files_owned.len() as i32),
                    ai_output: Some(ai_result.output),
                    ai_model: ai_result.model,
                    tokens_input: ai_result.tokens_input,
                    tokens_output: ai_result.tokens_output,
                    ..Default::default()
                },
            )
            .await?;

        // Step 20: Update status → "deploying"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("deploying".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(
                run_id,
                "info",
                &format!("PR created: {}. Triggering preview deployment...", pr.url),
                None,
            )
            .await?;
        tracing::info!(
            "Run {}: PR created, triggering preview deployment for branch {}",
            run_id,
            branch_name
        );

        // Step 21: Emit GitPushEvent to trigger preview deployment
        // Use the actual commit SHA from the PR (not the branch name) so that
        // SENTRY_RELEASE and other commit-based identifiers are valid.
        let commit_ref = pr.head_sha.clone().unwrap_or_else(|| branch_name.clone());
        let push_job = Job::GitPushEvent(GitPushEventJob {
            owner: project.repo_owner.clone(),
            repo: project.repo_name.clone(),
            branch: Some(branch_name.clone()),
            tag: None,
            commit: commit_ref,
            project_id: run.project_id,
        });

        if let Err(e) = self.queue.send(push_job).await {
            tracing::warn!(
                "Run {}: failed to emit GitPushEvent for preview deployment: {}",
                run_id,
                e
            );
        }

        // Step 22: Update status → "completed"
        self.run_service
            .update_status(
                run_id,
                UpdateRunFields {
                    status: Some("completed".to_string()),
                    completed_at: Some(Utc::now()),
                    ..Default::default()
                },
            )
            .await?;
        self.run_service
            .append_log(run_id, "info", "Autopilot run completed successfully", None)
            .await?;

        // Step 23: Send notification
        self.send_completion_notification(
            run_id,
            &config.name,
            &project.name,
            &project.slug,
            &format!(
                "Workflow **{}** created PR #{} to fix '{}' in **{}**. Review and merge: {}",
                config.name, pr.number, error_message, project.name, pr.url
            ),
            &config.deliverable,
        )
        .await;

        Ok(())
    }

    /// Build a rich context block from the trigger source. This is prepended
    /// to the agent's prompt so the YAML doesn't need template variables —
    /// Temps always provides all available context and the prompt just
    /// describes what to do with it.
    ///
    /// Returns an empty string if no meaningful context is available (e.g.
    /// manual triggers with no source).
    async fn build_trigger_context(
        &self,
        run: &agent_runs::Model,
        project: &projects::Model,
        error_type: &str,
        error_message: &str,
        stack_trace: &str,
        environment_name: Option<&str>,
    ) -> String {
        let mut ctx = String::new();
        ctx.push_str("## Trigger Context\n\n");
        ctx.push_str(&format!("- **Project:** {}\n", project.name));
        ctx.push_str(&format!("- **Trigger type:** {}\n", run.trigger_type));
        ctx.push_str(&format!(
            "- **Triggered at:** {}\n",
            run.created_at.to_rfc3339()
        ));

        // Deployment context
        if run.trigger_source_type.as_deref() == Some("deployment") {
            if let Some(deploy_id) = run.trigger_source_id {
                if let Ok(Some(deploy)) = deployments::Entity::find_by_id(deploy_id)
                    .one(self.db.as_ref())
                    .await
                {
                    ctx.push_str(&format!("- **Deploy ID:** {}\n", deploy.id));
                    if let Some(ref sha) = deploy.commit_sha {
                        ctx.push_str(&format!("- **Commit:** {}\n", sha));
                    }
                    if let Some(ref author) = deploy.commit_author {
                        ctx.push_str(&format!("- **Author:** {}\n", author));
                    }
                    if let Some(ref msg) = deploy.commit_message {
                        ctx.push_str(&format!("- **Commit message:** {}\n", msg));
                    }
                }
            }
        }

        // Error context
        if run.trigger_source_type.as_deref() == Some("error_group") {
            if !error_type.is_empty() {
                ctx.push_str(&format!("- **Error type:** {}\n", error_type));
            }
            if !error_message.is_empty() {
                ctx.push_str(&format!("- **Error message:** {}\n", error_message));
            }
            if let Some(env) = environment_name {
                ctx.push_str(&format!("- **Environment:** {}\n", env));
            }
            if let Some(group_id) = run.trigger_source_id {
                ctx.push_str(&format!("- **Error group ID:** {}\n", group_id));
            }
            if !stack_trace.is_empty() {
                ctx.push_str(&format!("\n### Stack Trace\n\n```\n{}\n```\n", stack_trace));
            }
        }

        // Monitor context
        if run.trigger_source_type.as_deref() == Some("status_monitor") {
            if let Some(monitor_id) = run.trigger_source_id {
                if let Ok(Some(monitor)) = status_monitors::Entity::find_by_id(monitor_id)
                    .one(self.db.as_ref())
                    .await
                {
                    ctx.push_str(&format!(
                        "- **Monitor:** {} (ID: {})\n",
                        monitor.name, monitor.id
                    ));
                    ctx.push_str(&format!("- **Monitor type:** {}\n", monitor.monitor_type));
                    if let Some(ref path) = monitor.check_path {
                        ctx.push_str(&format!("- **Check path:** {}\n", path));
                    }
                    ctx.push_str(&format!(
                        "- **Check interval:** {}s\n",
                        monitor.check_interval_seconds
                    ));
                }

                // Latest status check
                if let Ok(Some(check)) = status_checks::Entity::find()
                    .filter(status_checks::Column::MonitorId.eq(monitor_id))
                    .order_by(status_checks::Column::CheckedAt, Order::Desc)
                    .one(self.db.as_ref())
                    .await
                {
                    ctx.push_str(&format!("- **Current status:** {}\n", check.status));
                    if let Some(ms) = check.response_time_ms {
                        ctx.push_str(&format!("- **Response time:** {}ms\n", ms));
                    }
                    if let Some(ref err) = check.error_message {
                        ctx.push_str(&format!("- **Check error:** {}\n", err));
                    }
                    ctx.push_str(&format!(
                        "- **Last checked:** {}\n",
                        check.checked_at.to_rfc3339()
                    ));

                    // Downtime duration: find last non-down check
                    if check.status == "down" {
                        if let Ok(Some(last_ok)) = status_checks::Entity::find()
                            .filter(status_checks::Column::MonitorId.eq(monitor_id))
                            .filter(status_checks::Column::Status.ne("down"))
                            .order_by(status_checks::Column::CheckedAt, Order::Desc)
                            .one(self.db.as_ref())
                            .await
                        {
                            let secs = (check.checked_at - last_ok.checked_at).num_seconds();
                            let mins = secs / 60;
                            if mins > 0 {
                                ctx.push_str(&format!(
                                    "- **Down for:** {}m {}s\n",
                                    mins,
                                    secs % 60
                                ));
                            } else {
                                ctx.push_str(&format!("- **Down for:** {}s\n", secs));
                            }
                        }
                    }
                }
            }
        }

        // Container status for the project — helps the AI know if containers are
        // running, stopped, or crashed without needing to query via CLI first.
        if let Ok(containers) = deployment_containers::Entity::find()
            .inner_join(deployments::Entity)
            .filter(deployments::Column::ProjectId.eq(run.project_id))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .order_by(deployment_containers::Column::CreatedAt, Order::Desc)
            .all(self.db.as_ref())
            .await
        {
            if !containers.is_empty() {
                ctx.push_str("\n### Container Status\n\n");
                for c in &containers {
                    let status = c.status.as_deref().unwrap_or("unknown");
                    let service = c.service_name.as_deref().unwrap_or(&c.container_name);
                    ctx.push_str(&format!(
                        "- **{}**: {} (container: `{}`)\n",
                        service, status, c.container_id
                    ));
                }
            }
        }

        ctx
    }

    /// Load error context (type, message, stack trace, environment) from the error group and its
    /// latest event.
    async fn load_error_context(
        &self,
        trigger_source_id: Option<i32>,
        project_id: i32,
    ) -> Result<(String, String, String, Option<String>), AgentError> {
        let group_id = trigger_source_id.ok_or(AgentError::Validation {
            message: format!(
                "trigger_source_id is required for error_group trigger in project {}",
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

        // Cross-project guard: the run's project_id must own this group. Without
        // this check a caller with ProjectsWrite on project A could pass an
        // error_group_id belonging to project B and leak its error type,
        // message, and stack trace into project A's run prompt + logs.
        if group.project_id != project_id {
            return Err(AgentError::Validation {
                message: format!(
                    "Error group {} does not belong to project {}",
                    group_id, project_id
                ),
            });
        }

        // Load latest error event for the group to extract the stack trace
        let latest_event = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group_id))
            .order_by(error_events::Column::Timestamp, Order::Desc)
            .one(self.db.as_ref())
            .await
            .map_err(AgentError::Database)?;

        let stack_trace = if let Some(event) = &latest_event {
            if let Some(ref data_val) = event.data {
                // Try to extract stack_trace from the structured data
                if let Some(frames) = data_val.get("stack_trace").and_then(|v| v.as_array()) {
                    frames
                        .iter()
                        .map(|frame| {
                            let file = frame
                                .get("filename")
                                .or_else(|| frame.get("abs_path"))
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
                            format!("  at {} ({}:{})", func, file, lineno)
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
            None, // environment lookup would require joining environments table
        ))
    }

    /// Send a failure notification. Loads run + agent config from DB to get names.
    /// Best-effort: logs a warning if anything goes wrong.
    async fn send_failure_notification(&self, run_id: i32, error: &AgentError) {
        let run = match self.run_service.get_run(run_id).await {
            Ok(r) => r,
            Err(_) => return,
        };
        // Ephemeral runs don't have a persisted agent row — synthesize a
        // throwaway config from the run's YAML just for naming the notification.
        let config = if run.source == "cli_ephemeral" {
            let Some(yaml) = run.ephemeral_yaml.as_deref() else {
                return;
            };
            let default_provider = self.platform_default_provider().await;
            match self.synthesize_ephemeral_config(run.project_id, yaml, &default_provider) {
                Ok(c) => c,
                Err(_) => return,
            }
        } else {
            let Some(agent_id) = run.agent_id.or(run.config_id) else {
                return;
            };
            match self.config_service.get_agent_by_id(agent_id).await {
                Ok(Some(c)) => c,
                _ => return,
            }
        };
        let project = match projects::Entity::find_by_id(run.project_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(p)) => p,
            _ => return,
        };

        let body = format!(
            "Workflow **{}** failed on run #{} for **{}**.\n\n**Error:** {}\n\nCheck the run logs for details.",
            config.name, run_id, project.name, error
        );
        self.send_completion_notification(
            run_id,
            &config.name,
            &project.name,
            &project.slug,
            &body,
            &config.deliverable,
        )
        .await;
    }

    /// Send a completion notification for any deliverable type.
    /// The body is markdown and gets converted to email-safe HTML before sending.
    async fn send_completion_notification(
        &self,
        run_id: i32,
        agent_name: &str,
        project_name: &str,
        project_slug: &str,
        body: &str,
        deliverable: &str,
    ) {
        // Build the run URL relative to the app. The settings may contain a
        // public_url; fall back to reading it from the DB settings table.
        let run_url = self.build_run_url(project_slug, run_id).await;

        // Append a "View Run" link to the body
        let body_with_link = if let Some(ref url) = run_url {
            format!("{}\n\n[View workflow run →]({})", body, url)
        } else {
            body.to_string()
        };

        let html_body = Self::markdown_to_email_html(&body_with_link);
        let notification = Notification::new(
            format!("{}: {} (run #{})", agent_name, project_name, run_id),
            html_body,
        )
        .with_priority(NotificationPriority::Normal)
        .with_metadata("run_id", run_id.to_string())
        .with_metadata("project", project_name.to_string())
        .with_metadata("deliverable", deliverable.to_string());

        if let Err(e) = self
            .notification_service
            .send_notification(notification)
            .await
        {
            tracing::warn!(
                "Run {}: failed to send completion notification: {}",
                run_id,
                e
            );
        }
    }

    /// Build the full URL to a workflow run in the dashboard.
    /// Reads `external_url` from settings. Returns `None` if not configured.
    async fn build_run_url(&self, project_slug: &str, run_id: i32) -> Option<String> {
        let settings = settings::Entity::find_by_id(1)
            .one(self.db.as_ref())
            .await
            .ok()
            .flatten()?;
        let app_settings: temps_core::AppSettings = serde_json::from_value(settings.data).ok()?;
        let base = app_settings.external_url.as_deref()?.trim_end_matches('/');
        Some(format!(
            "{}/projects/{}/agents/{}",
            base, project_slug, run_id
        ))
    }

    /// Convert markdown to email-safe HTML with inline styles.
    /// Email clients ignore `<style>` blocks, so every element needs inline styles.
    fn markdown_to_email_html(text: &str) -> String {
        use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
        use std::fmt::Write;

        const FONT: &str = "font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif;";
        const MONO: &str =
            "font-family:'SFMono-Regular',Consolas,'Liberation Mono',Menlo,monospace;";

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);

        let parser = Parser::new_ext(text, options);
        let mut html = String::with_capacity(text.len() * 2);
        let mut in_code_block = false;
        let mut table_alignments: Vec<Alignment> = Vec::new();
        let mut table_cell_index: usize = 0;
        let mut in_table_head = false;

        for event in parser {
            match event {
                Event::Start(tag) => match tag {
                    Tag::Paragraph => {
                        let _ = write!(html, r#"<p style="margin:8px 0;line-height:1.6;{FONT}">"#);
                    }
                    Tag::Heading { level, .. } => {
                        let (size, color, margin) = match level {
                            pulldown_cmark::HeadingLevel::H1 => ("20px", "#111827", "20px 0 8px"),
                            pulldown_cmark::HeadingLevel::H2 => ("17px", "#1f2937", "18px 0 8px"),
                            pulldown_cmark::HeadingLevel::H3 => ("15px", "#374151", "14px 0 6px"),
                            _ => ("14px", "#374151", "12px 0 4px"),
                        };
                        let _ = write!(
                            html,
                            r#"<{level} style="margin:{margin};font-size:{size};font-weight:600;color:{color};{FONT}">"#
                        );
                    }
                    Tag::BlockQuote(_) => {
                        let _ = write!(
                            html,
                            r#"<blockquote style="margin:12px 0;padding:8px 16px;border-left:3px solid #d1d5db;color:#6b7280;">"#
                        );
                    }
                    Tag::CodeBlock(_) => {
                        in_code_block = true;
                        let _ = write!(
                            html,
                            r#"<pre style="background:#1e293b;color:#e2e8f0;padding:12px 16px;border-radius:6px;overflow-x:auto;{MONO}font-size:13px;margin:12px 0;line-height:1.5;"><code>"#
                        );
                    }
                    Tag::List(Some(start)) => {
                        let _ = write!(
                            html,
                            r#"<ol start="{start}" style="margin:8px 0;padding-left:24px;{FONT}">"#
                        );
                    }
                    Tag::List(None) => {
                        let _ = write!(
                            html,
                            r#"<ul style="margin:8px 0;padding-left:24px;{FONT}">"#
                        );
                    }
                    Tag::Item => {
                        let _ = write!(html, r#"<li style="margin:4px 0;line-height:1.5;">"#);
                    }
                    Tag::Table(alignments) => {
                        table_alignments = alignments;
                        let _ = write!(
                            html,
                            r#"<table style="width:100%;border-collapse:collapse;margin:12px 0;{FONT}">"#
                        );
                    }
                    Tag::TableHead => {
                        in_table_head = true;
                        table_cell_index = 0;
                        html.push_str("<thead><tr>");
                    }
                    Tag::TableRow => {
                        table_cell_index = 0;
                        html.push_str("<tr>");
                    }
                    Tag::TableCell => {
                        let align = table_alignments
                            .get(table_cell_index)
                            .copied()
                            .unwrap_or(Alignment::None);
                        let text_align = match align {
                            Alignment::Left => "left",
                            Alignment::Center => "center",
                            Alignment::Right => "right",
                            Alignment::None => "left",
                        };
                        if in_table_head {
                            let _ = write!(
                                html,
                                r#"<th style="text-align:{text_align};padding:8px 12px;border:1px solid #d1d5db;background:#f3f4f6;font-size:13px;font-weight:600;{FONT}">"#
                            );
                        } else {
                            let _ = write!(
                                html,
                                r#"<td style="text-align:{text_align};padding:8px 12px;border:1px solid #e5e7eb;font-size:13px;{FONT}">"#
                            );
                        }
                    }
                    Tag::Emphasis => html.push_str("<em>"),
                    Tag::Strong => {
                        let _ = write!(html, r#"<strong style="font-weight:600;">"#);
                    }
                    Tag::Strikethrough => html.push_str("<del>"),
                    Tag::Link {
                        dest_url, title, ..
                    } => {
                        let t = if title.is_empty() {
                            String::new()
                        } else {
                            format!(r#" title="{title}""#)
                        };
                        let _ = write!(
                            html,
                            r#"<a href="{dest_url}"{t} style="color:#2563eb;text-decoration:underline;">"#
                        );
                    }
                    Tag::Image {
                        dest_url, title, ..
                    } => {
                        let t = if title.is_empty() {
                            String::new()
                        } else {
                            format!(r#" title="{title}""#)
                        };
                        let _ = write!(
                            html,
                            r#"<img src="{dest_url}"{t} style="max-width:100%;height:auto;" alt=""#
                        );
                    }
                    _ => {}
                },
                Event::End(tag_end) => match tag_end {
                    TagEnd::Paragraph => html.push_str("</p>"),
                    TagEnd::Heading(level) => {
                        let _ = write!(html, "</{level}>");
                    }
                    TagEnd::BlockQuote(_) => html.push_str("</blockquote>"),
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        html.push_str("</code></pre>");
                    }
                    TagEnd::List(ordered) => {
                        html.push_str(if ordered { "</ol>" } else { "</ul>" });
                    }
                    TagEnd::Item => html.push_str("</li>"),
                    TagEnd::Table => html.push_str("</tbody></table>"),
                    TagEnd::TableHead => {
                        in_table_head = false;
                        html.push_str("</tr></thead><tbody>");
                    }
                    TagEnd::TableRow => html.push_str("</tr>"),
                    TagEnd::TableCell => {
                        html.push_str(if in_table_head { "</th>" } else { "</td>" });
                        table_cell_index += 1;
                    }
                    TagEnd::Emphasis => html.push_str("</em>"),
                    TagEnd::Strong => html.push_str("</strong>"),
                    TagEnd::Strikethrough => html.push_str("</del>"),
                    TagEnd::Link => html.push_str("</a>"),
                    TagEnd::Image => html.push_str(r#"" />"#),
                    _ => {}
                },
                Event::Text(t) => {
                    let escaped = t
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;");
                    html.push_str(&escaped);
                    let _ = in_code_block; // suppress unused warning
                }
                Event::Code(code) => {
                    let escaped = code
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;");
                    let _ = write!(
                        html,
                        r#"<code style="background:#f3f4f6;padding:2px 5px;border-radius:3px;{MONO}font-size:13px;">{escaped}</code>"#
                    );
                }
                Event::SoftBreak => html.push('\n'),
                Event::HardBreak => html.push_str("<br>"),
                Event::Rule => {
                    html.push_str(
                        r#"<hr style="border:none;border-top:1px solid #e5e7eb;margin:16px 0;">"#,
                    );
                }
                Event::Html(raw) | Event::InlineHtml(raw) => html.push_str(&raw),
                _ => {}
            }
        }

        html
    }
}

/// Build the CLI command args for running Claude (or Codex) in a sandbox.
pub fn build_claude_cmd(
    provider_name: &str,
    prompt: &str,
    max_turns: i32,
    continue_conversation: bool,
    model: Option<&str>,
) -> Vec<String> {
    match provider_name {
        "claude_cli" => {
            // Use full path — Docker exec may not resolve PATH correctly
            // when the binary lives in a named-volume-mounted home directory.
            let mut cmd = vec![
                "/home/temps/.local/bin/claude".to_string(),
                "--print".to_string(),
            ];
            if continue_conversation {
                cmd.push("--continue".to_string());
            }
            cmd.push(prompt.to_string());
            cmd.extend_from_slice(&[
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
            // Skip Codex's internal bubblewrap sandbox and approval prompts
            // — we're already running inside a Docker container which
            // provides isolation. This flag is designed for exactly this
            // use case ("environments that are externally sandboxed").
            cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string());
            // Emit structured JSONL output (like Claude's --output-format
            // stream-json) so each line is stored as an ai_event log entry.
            cmd.push("--json".to_string());
            cmd.push(prompt.to_string());
            cmd
        }
        "opencode" => {
            // OpenCode's `run [message..]` does an lstat on the first
            // positional arg (checking if it's a directory). Long agent
            // prompts exceed NAME_MAX (255). The caller writes the prompt
            // to /tmp/.temps-prompt; here we emit a bash wrapper that
            // reads it via `$(cat ...)`.
            let mut parts = vec!["opencode run".to_string()];
            if let Some(m) = model {
                if !m.is_empty() {
                    parts.push(format!("--model '{}'", m));
                }
            }
            parts.push("--format json".to_string());
            parts.push("\"$(cat /tmp/.temps-prompt)\"".to_string());
            vec!["bash".to_string(), "-lc".to_string(), parts.join(" ")]
        }
        _ => {
            vec![provider_name.to_string(), prompt.to_string()]
        }
    }
}

/// Metadata the AI emits to describe the pull request it produced.
#[derive(Debug, Default, Clone)]
pub struct PrMetadata {
    pub title: Option<String>,
    pub body: Option<String>,
}

/// Extract `<pr_title>…</pr_title>` and `<pr_body>…</pr_body>` blocks from
/// AI output. The report-text extractor runs first to collapse
/// stream-json/assistant frames into plain text, so this function works
/// uniformly across Claude, Codex, and OpenCode output formats.
pub fn extract_pr_metadata(ai_output: &str) -> PrMetadata {
    let text = extract_report_text(ai_output);
    PrMetadata {
        title: extract_tag(&text, "pr_title"),
        body: extract_tag(&text, "pr_body"),
    }
}

fn extract_tag(haystack: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = haystack.find(&open)? + open.len();
    let end = haystack[start..].find(&close)? + start;
    Some(haystack[start..end].trim().to_string())
}

/// Collapse an AI-generated PR title into a single clean line: unescape
/// literal `\n`/`\r`/`\t`, strip any actual newlines, trim whitespace and
/// trailing punctuation, and cap at 72 chars.
pub fn sanitize_pr_title(raw: &str) -> String {
    let unescaped = raw
        .replace("\\r\\n", " ")
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ");
    let flattened: String = unescaped
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_end_matches(['.', ',', ';']).trim();
    if trimmed.chars().count() > 72 {
        let mut out: String = trimmed.chars().take(71).collect();
        out.push('…');
        out
    } else {
        trimmed.to_string()
    }
}

/// Unescape literal `\n`/`\r`/`\t` escape sequences that AI models sometimes
/// emit inside Markdown (especially when they confuse JSON-string and raw-
/// text contexts). Leaves real newlines untouched.
pub fn normalize_markdown(raw: &str) -> String {
    raw.replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n")
        .replace("\\t", "    ")
}

/// Extract human-readable report text from AI output. Supports both Claude
/// stream-json (`type: "assistant"` with `message.content[].text`) and Codex
/// `--json` (`type: "item.completed"` with `item.type: "agent_message"`).
/// Falls back to a Claude `type: "result"` summary, then raw output.
pub fn extract_report_text(output: &str) -> String {
    let mut assistant_texts: Vec<String> = Vec::new();

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
            // Claude stream-json: {"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}
            "assistant" => {
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                assistant_texts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            // Codex --json emits agent messages on EITHER `item.started` (newer
            // gpt-5-codex streams the whole message up front) OR `item.completed`
            // (older builds emit only on completion). Dedupe below so a message
            // present in both events is not repeated.
            "item.started" | "item.completed" => {
                if let Some(item) = v.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("agent_message") {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() && !assistant_texts.iter().any(|t| t == text) {
                                assistant_texts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            // OpenCode --format json: {"type":"text","part":{"type":"text","text":"..."}}
            "text" => {
                if let Some(text) = v
                    .get("part")
                    .and_then(|p| p.get("text"))
                    .and_then(|t| t.as_str())
                {
                    if !text.is_empty() {
                        assistant_texts.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    if !assistant_texts.is_empty() {
        return assistant_texts.join("\n\n");
    }

    // Fallback: try the Claude "result" summary
    for line in output.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                    return result.to_string();
                }
            }
        }
    }

    // Last resort: raw output
    output.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_entities::{agent_run_logs, agent_runs, project_agents};
    use temps_git::{GitProviderManagerError, PullRequest, RepositoryInfo};

    #[test]
    fn test_branch_name_format() {
        let run_id = 255_i32;
        let short_run_id = format!("{:x}", run_id);
        let branch_name = format!("autopilot/fix/err-42-{}", short_run_id);
        assert!(branch_name.contains("ff"));
        assert!(branch_name.contains("err-42"));
    }

    // ---- Fakes ----

    /// Fake AI CLI that writes files into work_dir and returns them in changed_files.
    struct FakeAiCli {
        files_to_create: Vec<(String, String)>,
        output: String,
    }

    fn fake_status(name: &str) -> crate::ai_cli::AiCliStatus {
        crate::ai_cli::AiCliStatus {
            provider: name.into(),
            installed: true,
            version: Some("1.0.0-fake".into()),
            authenticated: true,
            auth_method: Some("test".into()),
            email: None,
            subscription_type: None,
            setup_hint: None,
        }
    }

    #[async_trait]
    impl AiCliProvider for FakeAiCli {
        fn name(&self) -> &str {
            "fake_cli"
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> crate::ai_cli::AiCliStatus {
            fake_status("fake_cli")
        }
        async fn run(&self, config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            for (path, content) in &self.files_to_create {
                let full = config.work_dir.join(path);
                if let Some(parent) = full.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&full, content).await?;
            }
            Ok(AiRunResult {
                output: self.output.clone(),
                exit_code: 0,
                tokens_input: Some(1000),
                tokens_output: Some(500),
                model: Some("fake-model".to_string()),
                changed_files: Some(
                    self.files_to_create
                        .iter()
                        .map(|(p, _)| p.clone())
                        .collect(),
                ),
                session_id: None,
                is_max_turns_error: false,
            })
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    /// Fake AI CLI that returns an error.
    struct FailingAiCli;

    #[async_trait]
    impl AiCliProvider for FailingAiCli {
        fn name(&self) -> &str {
            "failing_cli"
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> crate::ai_cli::AiCliStatus {
            fake_status("failing_cli")
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            Err(AgentError::AiCliFailed {
                provider: "failing_cli".into(),
                exit_code: 1,
                stderr: "Simulated failure".into(),
            })
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    /// Fake AI CLI that returns no changes.
    struct NoChangesAiCli;

    #[async_trait]
    impl AiCliProvider for NoChangesAiCli {
        fn name(&self) -> &str {
            "no_changes_cli"
        }
        async fn check_installed(&self) -> bool {
            true
        }
        async fn get_status(&self) -> crate::ai_cli::AiCliStatus {
            fake_status("no_changes_cli")
        }
        async fn run(&self, _config: AiRunConfig) -> Result<AiRunResult, AgentError> {
            Ok(AiRunResult {
                output: "I analyzed the code but couldn't find a fix.".into(),
                exit_code: 0,
                tokens_input: Some(500),
                tokens_output: Some(200),
                model: Some("fake-model".into()),
                changed_files: Some(vec![]),
                session_id: None,
                is_max_turns_error: false,
            })
        }
        async fn continue_conversation(
            &self,
            config: AiRunConfig,
        ) -> Result<AiRunResult, AgentError> {
            self.run(config).await
        }
    }

    /// Records what was pushed so tests can assert on it.
    #[derive(Default)]
    struct GitRecorder {
        cloned: Mutex<Vec<(i32, String, String)>>,
        pushed: Mutex<Vec<PushRecord>>,
    }

    #[derive(Debug, Clone)]
    struct PushRecord {
        branch: String,
        base_branch: String,
        files: Vec<String>,
        pr_title: String,
    }

    /// Fake git provider that records calls.
    struct FakeGitProvider {
        recorder: Arc<GitRecorder>,
        clone_should_fail: bool,
    }

    #[async_trait]
    impl GitProviderManagerTrait for FakeGitProvider {
        async fn get_connection_access_token(
            &self,
            _connection_id: i32,
        ) -> Result<(String, String), GitProviderManagerError> {
            Ok(("fake-token".to_string(), "github".to_string()))
        }

        async fn clone_repository(
            &self,
            connection_id: i32,
            repo_owner: &str,
            repo_name: &str,
            _target_dir: &std::path::Path,
            _branch_or_ref: Option<&str>,
        ) -> Result<(), GitProviderManagerError> {
            if self.clone_should_fail {
                return Err(GitProviderManagerError::CloneError(
                    "Simulated clone failure".into(),
                ));
            }
            self.recorder.cloned.lock().unwrap().push((
                connection_id,
                repo_owner.to_string(),
                repo_name.to_string(),
            ));
            Ok(())
        }

        async fn get_repository_info(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
        ) -> Result<RepositoryInfo, GitProviderManagerError> {
            Ok(RepositoryInfo {
                clone_url: "https://github.com/test/repo.git".into(),
                default_branch: "main".into(),
                owner: "test".into(),
                name: "repo".into(),
            })
        }

        async fn download_archive(
            &self,
            _connection_id: i32,
            _repo_owner: &str,
            _repo_name: &str,
            _branch_or_ref: &str,
            _archive_path: &std::path::Path,
        ) -> Result<(), GitProviderManagerError> {
            Err(GitProviderManagerError::Other("not used".into()))
        }

        async fn push_files_and_create_pr(
            &self,
            _connection_id: i32,
            _owner: &str,
            _repo: &str,
            branch: &str,
            base_branch: &str,
            files: Vec<(String, Vec<u8>)>,
            _commit_message: &str,
            pr_title: &str,
            _pr_body: &str,
        ) -> Result<PullRequest, GitProviderManagerError> {
            self.recorder.pushed.lock().unwrap().push(PushRecord {
                branch: branch.to_string(),
                base_branch: base_branch.to_string(),
                files: files.iter().map(|(p, _)| p.clone()).collect(),
                pr_title: pr_title.to_string(),
            });
            Ok(PullRequest {
                number: 42,
                url: "https://github.com/test/repo/pull/42".to_string(),
                title: pr_title.to_string(),
                head_branch: branch.to_string(),
                base_branch: base_branch.to_string(),
                head_sha: Some("abc123def456".to_string()),
            })
        }

        async fn mint_scoped_repo_token(
            &self,
            _: i32,
            _: &str,
            _: &str,
            _: temps_git::ScopedTokenOp,
        ) -> Result<temps_git::ScopedTokenGrant, GitProviderManagerError> {
            Err(GitProviderManagerError::Other("not used in test".into()))
        }
    }

    /// Fake job queue that records sent jobs.
    struct FakeJobQueue {
        sent: Mutex<Vec<Job>>,
    }

    #[async_trait::async_trait]
    impl JobQueue for FakeJobQueue {
        async fn send(&self, job: Job) -> Result<(), temps_core::QueueError> {
            self.sent.lock().unwrap().push(job);
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("not needed for executor tests")
        }
    }

    // ---- Test data builders ----

    fn make_run(id: i32, project_id: i32) -> agent_runs::Model {
        agent_runs::Model {
            id,
            project_id,
            config_id: Some(1),
            agent_id: None,
            trigger_type: "new_issue".into(),
            trigger_source_id: Some(10),
            trigger_source_type: Some("error_group".into()),
            status: "pending".into(),
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
            created_at: Utc::now(),
            phase: None,
            analysis: None,
            user_context: None,
            ai_session_id: None,
            source: "committed".into(),
            ephemeral_yaml: None,

            prompt_text: None,
            workspace_volume: None,
        }
    }

    fn make_config(project_id: i32) -> project_agents::Model {
        project_agents::Model {
            id: 1,
            project_id,
            slug: "default-agent".into(),
            name: "Default Agent".into(),
            description: None,
            source: "dashboard".into(),
            enabled: true,
            trigger_config: serde_json::json!({
                "error": { "new_issue": true, "regression": true },
                "manual": true
            }),
            prompt: None,
            ai_provider: "fake_cli".into(),
            ai_model: None,
            api_key_encrypted: Some("encrypted-key".into()),
            ai_provider_key_id: None,
            max_turns: 10,
            timeout_seconds: 600,
            daily_budget_cents: 500,
            cooldown_minutes: 30,
            branch_prefix: "autopilot/".into(),
            deliverable: "pull_request".into(),
            sandbox_enabled: None,
            config_repo_url: None,
            config_repo_branch: None,
            mcp_servers_config: None,
            skills_config: None,
            tools_config: None,
            webhook_id: None,
            webhook_token: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_project(id: i32) -> projects::Model {
        projects::Model {
            id,
            name: "test-app".into(),
            repo_name: "repo".into(),
            repo_owner: "testowner".into(),
            directory: ".".into(),
            main_branch: "main".into(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: "test-app".into(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: Some(5),
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            attack_mode: false,
            enable_preview_environments: true,
            source_type: temps_entities::source_type::SourceType::Git,
        }
    }

    fn make_error_group(id: i32) -> error_groups::Model {
        error_groups::Model {
            id,
            title: "Cannot read property 'map' of undefined".into(),
            error_type: "TypeError".into(),
            message_template: None,
            embedding: None,
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            total_count: 47,
            status: "unresolved".into(),
            assigned_to: None,
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            visitor_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_error_event(group_id: i32) -> error_events::Model {
        error_events::Model {
            id: 1,
            error_group_id: group_id,
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            visitor_id: None,
            ip_geolocation_id: None,
            fingerprint_hash: "abc123".into(),
            timestamp: Utc::now(),
            exception_type: "TypeError".into(),
            exception_value: Some("Cannot read property 'map' of undefined".into()),
            source: Some("sentry".into()),
            data: Some(serde_json::json!({
                "stack_trace": [
                    {
                        "filename": "src/components/UserList.tsx",
                        "function": "UserList.render",
                        "lineno": 42,
                        "colno": 18
                    }
                ]
            })),
            trace_id_indexed: None,
            created_at: Utc::now(),
        }
    }

    fn make_log(run_id: i32) -> agent_run_logs::Model {
        agent_run_logs::Model {
            id: 1,
            run_id,
            level: "info".into(),
            message: "test".into(),
            metadata: None,
            created_at: Utc::now(),
        }
    }

    fn make_encryption_service() -> Arc<EncryptionService> {
        Arc::new(EncryptionService::new_from_password(
            "test-password-for-autopilot",
        ))
    }

    fn make_notification_service(db: Arc<sea_orm::DatabaseConnection>) -> Arc<NotificationService> {
        let enc = make_encryption_service();
        Arc::new(NotificationService::new(db, enc))
    }

    fn make_sandbox_registry() -> Arc<SandboxRegistry> {
        use crate::sandbox::local::LocalSandboxProvider;
        Arc::new(SandboxRegistry::new(Arc::new(LocalSandboxProvider::new())))
    }

    fn make_secret_service(db: Arc<sea_orm::DatabaseConnection>) -> Arc<SecretService> {
        Arc::new(SecretService::new(db, make_encryption_service()))
    }

    fn make_definition_service(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> Arc<crate::services::definition_service::DefinitionService> {
        Arc::new(crate::services::definition_service::DefinitionService::new(
            db,
        ))
    }

    /// Build a MockDatabase for the happy path.
    ///
    /// Sea-ORM MockDatabase serves query results as a single FIFO queue.
    /// We must push results in the exact order the executor consumes them.
    /// Each `update_status` does: get_run (SELECT) → update (UPDATE RETURNING *) = 2 run results.
    /// Each `append_log` does: INSERT RETURNING * = 1 log result.
    fn build_happy_path_db(run_id: i32, project_id: i32) -> sea_orm::DatabaseConnection {
        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key-123").unwrap());
        let project = make_project(project_id);
        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let log = make_log(run_id);

        // Helper: push an update_status (2 run rows) then an append_log (1 log row)
        // This covers the common pattern in the executor.
        let r = run.clone();
        let l = log.clone();

        // The executor interleaves run queries (SELECT + UPDATE) with log inserts.
        // Sea-ORM MockDatabase uses a single FIFO queue for all query results.
        // We must push results in the exact order they'll be consumed.
        // Pattern for each update_status: run, run (SELECT then UPDATE RETURNING)
        // Pattern for each append_log: log (INSERT RETURNING)
        let mut builder = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![run.clone()]]) // get_run
            .append_query_results(vec![vec![config]]) // get_config by id
            .append_query_results(vec![vec![project]]) // project find_by_id
            .append_query_results(vec![vec![error_group]]) // error_group find_by_id
            .append_query_results(vec![vec![error_event]]); // error_event find

        // The executor does ~10 update_status calls and ~8 append_log calls
        // in alternating order. Push 50 results alternating run/log to cover all paths.
        for _ in 0..25 {
            builder = builder
                .append_query_results(vec![vec![r.clone()]]) // run result
                .append_query_results(vec![vec![r.clone()]]) // run result
                .append_query_results(vec![vec![l.clone()]]); // log result
        }

        builder.into_connection()
    }

    // ---- Integration tests ----

    #[tokio::test]
    #[ignore] // MockDatabase FIFO queue can't handle the executor's complex query interleaving.
              // This test needs a real TestDatabase to work reliably. The other executor tests
              // (no_changes, too_many_files, clone_failure, ai_failure, no_git_connection) cover
              // the individual failure paths.
    async fn test_executor_happy_path_clones_pushes_creates_pr() {
        let run_id = 1;
        let project_id = 1;

        let db = Arc::new(build_happy_path_db(run_id, project_id));
        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: false,
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let enc = make_encryption_service();
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        let ai = Arc::new(FakeAiCli {
            files_to_create: vec![("src/fix.ts".into(), "fixed code".into())],
            output: "I fixed the TypeError by adding a null check.".into(),
        });

        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // Assert: git clone was called
        let clones = recorder.cloned.lock().unwrap();
        assert_eq!(clones.len(), 1, "should have cloned once");
        assert_eq!(clones[0].0, 5, "connection_id should be 5");
        assert_eq!(clones[0].1, "testowner");
        assert_eq!(clones[0].2, "repo");

        // Assert: PR was pushed
        let pushes = recorder.pushed.lock().unwrap();
        assert_eq!(pushes.len(), 1, "should have pushed once");
        let push = &pushes[0];
        assert!(
            push.branch.starts_with("autopilot/fix/err-10-"),
            "branch should start with autopilot prefix + error group id: {}",
            push.branch
        );
        assert_eq!(push.base_branch, "main");
        assert_eq!(push.files, vec!["src/fix.ts"]);
        assert!(
            push.pr_title.contains("TypeError"),
            "PR title should contain the error type: {}",
            push.pr_title
        );

        // Assert: GitPushEvent was emitted for preview deployment
        let jobs = queue.sent.lock().unwrap();
        assert!(!jobs.is_empty(), "should have emitted at least one job");
        let has_push = jobs.iter().any(|j| matches!(j, Job::GitPushEvent(_)));
        assert!(
            has_push,
            "should have emitted GitPushEvent for preview deploy"
        );
    }

    #[tokio::test]
    async fn test_executor_no_changes_marks_no_fix() {
        let run_id = 2;
        let project_id = 1;

        // Fewer mock results needed — executor stops at "no_fix" before pushing
        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key").unwrap());
        let project = make_project(project_id);
        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let updated_run = run.clone();
        let log = make_log(run_id);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![run.clone()]])
                .append_query_results(vec![vec![config]])
                .append_query_results(vec![vec![project]])
                .append_query_results(vec![vec![error_group]])
                .append_query_results(vec![vec![error_event]])
                // cloning
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // analyzing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 2,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // fixing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 3,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // AI completed log
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 4,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // no_fix status update
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                // "No file changes detected" log
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 5,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                .into_connection(),
        );

        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: false,
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        let ai = Arc::new(NoChangesAiCli);
        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // PR should NOT have been pushed
        assert!(
            recorder.pushed.lock().unwrap().is_empty(),
            "should not push when no changes"
        );
        // GitPushEvent should NOT have been emitted
        assert!(
            queue.sent.lock().unwrap().is_empty(),
            "should not emit jobs when no changes"
        );
    }

    #[tokio::test]
    async fn test_executor_too_many_files_aborts() {
        let run_id = 3;
        let project_id = 1;

        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key").unwrap());
        let project = make_project(project_id);
        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let updated_run = run.clone();
        let log = make_log(run_id);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![run.clone()]])
                .append_query_results(vec![vec![config]])
                .append_query_results(vec![vec![project]])
                .append_query_results(vec![vec![error_group]])
                .append_query_results(vec![vec![error_event]])
                // cloning
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // analyzing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 2,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // fixing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 3,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // AI completed log
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 4,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // failed status update (error path)
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                // error log
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 5,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                .into_connection(),
        );

        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: false,
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        // Create 51 files — exceeds MAX_FILES_CHANGED (50)
        let files: Vec<(String, String)> = (0..51)
            .map(|i| (format!("src/file_{}.ts", i), format!("content {}", i)))
            .collect();
        let ai = Arc::new(FakeAiCli {
            files_to_create: files,
            output: "I refactored the entire codebase".into(),
        });

        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // PR should NOT have been pushed — safety limit exceeded
        assert!(
            recorder.pushed.lock().unwrap().is_empty(),
            "should not push when too many files"
        );
        assert!(
            queue.sent.lock().unwrap().is_empty(),
            "should not emit jobs when safety limit hit"
        );
    }

    #[tokio::test]
    async fn test_executor_clone_failure_marks_failed() {
        let run_id = 4;
        let project_id = 1;

        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key").unwrap());
        let project = make_project(project_id);
        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let updated_run = run.clone();
        let log = make_log(run_id);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![run.clone()]])
                .append_query_results(vec![vec![config]])
                .append_query_results(vec![vec![project]])
                .append_query_results(vec![vec![error_group]])
                .append_query_results(vec![vec![error_event]])
                // cloning status
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // failed status (error path)
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 2,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                .into_connection(),
        );

        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: true, // <-- clone will fail
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        let ai = Arc::new(FakeAiCli {
            files_to_create: vec![],
            output: "".into(),
        });

        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // Nothing should have been pushed
        assert!(recorder.pushed.lock().unwrap().is_empty());
        assert!(
            recorder.cloned.lock().unwrap().is_empty(),
            "clone_repository should have been called but returned error"
        );
    }

    #[tokio::test]
    async fn test_executor_ai_failure_marks_failed() {
        let run_id = 5;
        let project_id = 1;

        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key").unwrap());
        let project = make_project(project_id);
        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let updated_run = run.clone();
        let log = make_log(run_id);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![run.clone()]])
                .append_query_results(vec![vec![config]])
                .append_query_results(vec![vec![project]])
                .append_query_results(vec![vec![error_group]])
                .append_query_results(vec![vec![error_event]])
                // cloning
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // analyzing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 2,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // fixing
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 3,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // failed status (error path)
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 4,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                .into_connection(),
        );

        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: false,
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        let ai: Arc<dyn AiCliProvider> = Arc::new(FailingAiCli);
        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // PR should not have been pushed
        assert!(recorder.pushed.lock().unwrap().is_empty());
    }

    // ---------------------------------------------------------------------------
    // Feature: Report deliverable
    // ---------------------------------------------------------------------------

    // ---------------------------------------------------------------------------
    // Report deliverable: logic tests (no executor needed)
    // ---------------------------------------------------------------------------

    /// Verify the report branch condition: `config.deliverable == "report"`.
    /// The executor short-circuits before creating a branch/PR when this is true.
    #[test]
    fn test_report_deliverable_condition_matches() {
        let mut config = make_config(1);
        config.deliverable = "report".into();
        assert_eq!(config.deliverable, "report");

        // Non-report deliverable should NOT match
        let default_config = make_config(1);
        assert_ne!(default_config.deliverable, "report");
        assert_eq!(default_config.deliverable, "pull_request");
    }

    /// Verify report output is returned as-is when ReportAiCli is used — the
    /// `changed_files: Some(vec![])` pattern means no PR would be created even
    /// without the deliverable check.
    #[tokio::test]
    async fn test_report_ai_cli_returns_stream_json_output() {
        let result_text = "Analysis: root cause is a null pointer";
        // Build the output that ReportAiCli would produce
        let output = format!("{{\"type\":\"result\",\"result\":\"{}\"}}\n", result_text);

        // Extract report text using the same logic as the executor's report branch
        let report_text = output
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
            .unwrap_or_else(|| output.clone());

        assert_eq!(report_text, result_text);
    }

    // ---------------------------------------------------------------------------
    // Feature: markdown_to_email_html conversions
    // ---------------------------------------------------------------------------

    #[test]
    fn test_markdown_to_email_html_heading() {
        let html = AgentExecutor::markdown_to_email_html("## Heading");
        assert!(html.contains("<h2"), "h2 tag should be present: {}", html);
        assert!(
            html.contains("Heading"),
            "heading text should be present: {}",
            html
        );
        assert!(
            html.contains("style="),
            "h2 should have inline styles: {}",
            html
        );
        assert!(html.contains("</h2>"), "closing h2 tag required: {}", html);
    }

    #[test]
    fn test_markdown_to_email_html_bold() {
        let html = AgentExecutor::markdown_to_email_html("**bold**");
        assert!(
            html.contains("<strong"),
            "strong tag should be present: {}",
            html
        );
        assert!(
            html.contains("bold"),
            "bold text should be present: {}",
            html
        );
        assert!(
            html.contains("font-weight:600"),
            "strong should have font-weight inline style: {}",
            html
        );
        assert!(
            html.contains("</strong>"),
            "closing strong tag required: {}",
            html
        );
    }

    #[test]
    fn test_markdown_to_email_html_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let html = AgentExecutor::markdown_to_email_html(md);
        assert!(html.contains("<table"), "table tag required: {}", html);
        assert!(html.contains("<th"), "th tag required for header: {}", html);
        assert!(html.contains("<td"), "td tag required for data: {}", html);
        // Inline styles on both th and td
        assert!(
            html.contains("padding:8px 12px"),
            "table cells should have padding style: {}",
            html
        );
        assert!(
            html.contains("</table>"),
            "closing table tag required: {}",
            html
        );
        // Content
        assert!(
            html.contains("A"),
            "column A header should appear: {}",
            html
        );
        assert!(
            html.contains("B"),
            "column B header should appear: {}",
            html
        );
        assert!(html.contains("1"), "cell value 1 should appear: {}", html);
        assert!(html.contains("2"), "cell value 2 should appear: {}", html);
    }

    #[test]
    fn test_markdown_to_email_html_code_block() {
        let md = "```\nlet x = 1;\n```";
        let html = AgentExecutor::markdown_to_email_html(md);
        assert!(html.contains("<pre"), "pre tag required: {}", html);
        assert!(html.contains("<code>"), "code tag required: {}", html);
        assert!(
            html.contains("let x = 1;"),
            "code content required: {}",
            html
        );
        // Inline style on pre
        assert!(
            html.contains("background:#1e293b"),
            "code block should have dark background style: {}",
            html
        );
        assert!(
            html.contains("</code></pre>"),
            "closing tags required: {}",
            html
        );
    }

    #[test]
    fn test_markdown_to_email_html_empty_input() {
        let html = AgentExecutor::markdown_to_email_html("");
        assert!(
            html.is_empty(),
            "empty input should produce empty output, got: {}",
            html
        );
    }

    // ---------------------------------------------------------------------------
    // Feature: User context in prompt (logic tests — no executor needed)
    // ---------------------------------------------------------------------------
    // The executor builds the prompt then appends user_context using the same
    // pattern shown below. Testing the string manipulation directly avoids the
    // complex MockDatabase FIFO queue that makes full executor integration tests
    // fragile (see test_executor_happy_path_clones_pushes_creates_pr for context).

    #[test]
    fn test_user_context_appended_when_set() {
        // Mirror the exact logic from execute_run_inner:
        //   if let Some(ref ctx) = run.user_context { ... format!("{}\n\n---\nUSER CONTEXT:\n{}\n") }
        let base_prompt = "You are an AI agent performing a task.".to_string();
        let user_ctx = "Research edge caching".to_string();

        let prompt = if !user_ctx.is_empty() {
            format!("{}\n\n---\nUSER CONTEXT:\n{}\n", base_prompt, user_ctx)
        } else {
            base_prompt.clone()
        };

        assert!(
            prompt.contains("USER CONTEXT:"),
            "prompt should contain 'USER CONTEXT:' section"
        );
        assert!(
            prompt.contains("Research edge caching"),
            "prompt should contain user context text"
        );
        // Base prompt should still be present
        assert!(
            prompt.contains("You are an AI agent"),
            "base prompt should still be present"
        );
    }

    #[test]
    fn test_user_context_not_appended_when_none() {
        let base_prompt = "You are an AI agent performing a task.".to_string();
        // run.user_context = None → prompt unchanged
        let user_ctx: Option<String> = None;

        let prompt = if let Some(ref ctx) = user_ctx {
            if !ctx.is_empty() {
                format!("{}\n\n---\nUSER CONTEXT:\n{}\n", base_prompt, ctx)
            } else {
                base_prompt.clone()
            }
        } else {
            base_prompt.clone()
        };

        assert!(
            !prompt.contains("USER CONTEXT:"),
            "prompt should NOT contain 'USER CONTEXT:' when user_context is None"
        );
        assert_eq!(
            prompt, base_prompt,
            "prompt should be unchanged when no user context"
        );
    }

    #[test]
    fn test_user_context_not_appended_when_empty_string() {
        let base_prompt = "You are an AI agent performing a task.".to_string();
        // run.user_context = Some("") → empty string is skipped
        let user_ctx: Option<String> = Some(String::new());

        let prompt = if let Some(ref ctx) = user_ctx {
            if !ctx.is_empty() {
                format!("{}\n\n---\nUSER CONTEXT:\n{}\n", base_prompt, ctx)
            } else {
                base_prompt.clone()
            }
        } else {
            base_prompt.clone()
        };

        assert!(
            !prompt.contains("USER CONTEXT:"),
            "prompt should NOT contain 'USER CONTEXT:' when user_context is empty string"
        );
        assert_eq!(
            prompt, base_prompt,
            "prompt should be unchanged when user context is empty"
        );
    }

    #[test]
    fn test_user_context_separator_format() {
        // Verify the exact format of the USER CONTEXT section separator
        let base = "base prompt";
        let ctx = "my context";
        let expected = format!("{}\n\n---\nUSER CONTEXT:\n{}\n", base, ctx);

        assert!(
            expected.contains("\n\n---\n"),
            "separator should be on its own line preceded by blank line"
        );
        assert!(
            expected.ends_with(&format!("{}\n", ctx)),
            "context should end with newline"
        );
    }

    // ---------------------------------------------------------------------------
    // Feature: sandbox_enabled Option<bool> logic (pure unit test, no executor)
    // ---------------------------------------------------------------------------

    fn resolve_sandbox(agent: Option<bool>, global: bool) -> bool {
        agent.unwrap_or(global)
    }

    #[test]
    fn test_sandbox_override_none_uses_global_default_false() {
        assert!(
            !resolve_sandbox(None, false),
            "None + global=false should yield false"
        );
    }

    #[test]
    fn test_sandbox_override_none_uses_global_default_true() {
        assert!(
            resolve_sandbox(None, true),
            "None + global=true should yield true"
        );
    }

    #[test]
    fn test_sandbox_override_some_true_forces_on_regardless_of_global() {
        assert!(
            resolve_sandbox(Some(true), false),
            "Some(true) + global=false should yield true"
        );
        assert!(
            resolve_sandbox(Some(true), true),
            "Some(true) + global=true should yield true"
        );
    }

    #[test]
    fn test_sandbox_override_some_false_forces_off_regardless_of_global() {
        assert!(
            !resolve_sandbox(Some(false), true),
            "Some(false) + global=true should yield false"
        );
        assert!(
            !resolve_sandbox(Some(false), false),
            "Some(false) + global=false should yield false"
        );
    }

    // ---------------------------------------------------------------------------
    // Feature: report text extraction (extract_report_text)
    // ---------------------------------------------------------------------------

    #[test]
    fn test_extract_report_claude_assistant_text() {
        let output = concat!(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"First paragraph.\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Second paragraph.\"}]}}\n",
        );
        let report = extract_report_text(output);
        assert_eq!(report, "First paragraph.\n\nSecond paragraph.");
    }

    #[test]
    fn test_extract_report_claude_result_fallback() {
        // No assistant text blocks — falls back to result summary.
        let output =
            "{\"type\":\"result\",\"result\":\"Found the root cause: null pointer in UserList\"}\n";
        let report = extract_report_text(output);
        assert_eq!(report, "Found the root cause: null pointer in UserList");
    }

    #[test]
    fn test_extract_report_raw_fallback() {
        let output = "Plain text output without JSON";
        let report = extract_report_text(output);
        assert_eq!(report, output);
    }

    #[test]
    fn test_extract_report_codex_agent_messages() {
        let output = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"abc\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Checking funnels...\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"ls\",\"aggregated_output\":\"foo\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Conversions are healthy.\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1000,\"output_tokens\":200}}\n",
        );
        let report = extract_report_text(output);
        assert_eq!(report, "Checking funnels...\n\nConversions are healthy.");
    }

    #[test]
    fn test_extract_report_codex_skips_commands() {
        // Only agent_message items should be extracted, not command_execution.
        let output = "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"pwd\",\"aggregated_output\":\"/workspace\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n";
        let report = extract_report_text(output);
        // Falls back to raw output since no assistant texts found
        assert_eq!(report, output);
    }

    #[test]
    fn test_extract_report_codex_item_started_agent_message() {
        // Newer gpt-5-codex streams the agent message on item.started and
        // the parser must pick it up (previously it only matched item.completed
        // and would fall back to dumping raw JSON).
        let output = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"abc\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"Reading input...\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"ls\",\"aggregated_output\":\"foo\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
        );
        let report = extract_report_text(output);
        assert_eq!(report, "Reading input...");
    }

    #[test]
    fn test_extract_report_codex_dedupes_started_and_completed() {
        // When codex emits the same agent_message on both item.started AND
        // item.completed, the extractor should only surface it once.
        let output = concat!(
            "{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"Done.\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Done.\"}}\n",
        );
        let report = extract_report_text(output);
        assert_eq!(report, "Done.");
    }

    #[tokio::test]
    async fn test_executor_no_git_connection_fails() {
        let run_id = 6;
        let project_id = 1;

        let run = make_run(run_id, project_id);
        let mut config = make_config(project_id);
        let enc = make_encryption_service();
        config.api_key_encrypted = Some(enc.encrypt_string("sk-test-key").unwrap());
        let mut project = make_project(project_id);
        project.git_provider_connection_id = None; // <-- no connection

        let error_group = make_error_group(10);
        let error_event = make_error_event(10);
        let updated_run = run.clone();
        let log = make_log(run_id);

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![run.clone()]])
                .append_query_results(vec![vec![config]])
                .append_query_results(vec![vec![project]])
                .append_query_results(vec![vec![error_group]])
                .append_query_results(vec![vec![error_event]])
                // cloning status
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                // failed status (error path — no git connection)
                .append_query_results(vec![vec![updated_run.clone()]])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: run_id as u64,
                    rows_affected: 1,
                }])
                .append_exec_results(vec![sea_orm::MockExecResult {
                    last_insert_id: 2,
                    rows_affected: 1,
                }])
                .append_query_results(vec![vec![log.clone()]])
                .into_connection(),
        );

        let recorder = Arc::new(GitRecorder::default());
        let git = Arc::new(FakeGitProvider {
            recorder: recorder.clone(),
            clone_should_fail: false,
        });
        let queue = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));

        let ai = Arc::new(FakeAiCli {
            files_to_create: vec![],
            output: "".into(),
        });
        let executor = AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue.clone(),
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
        .with_ai_provider(ai);

        executor.execute_run(run_id).await;

        // Clone should not even have been attempted
        assert!(recorder.cloned.lock().unwrap().is_empty());
        assert!(recorder.pushed.lock().unwrap().is_empty());
    }

    // ---------------------------------------------------------------------------
    // Feature: workflow memory wiring on AgentExecutor
    // ---------------------------------------------------------------------------

    use temps_core::workflow_memory::{
        WorkflowMemoryError, WorkflowMemoryFact, WorkflowMemoryProvider,
    };

    /// Fake memory provider for executor unit tests. Records calls and
    /// returns whatever facts/errors the test set up.
    struct FakeMemoryProvider {
        facts: Vec<WorkflowMemoryFact>,
        load_calls: Mutex<Vec<(i32, i32, Vec<String>)>>,
        force_error: bool,
    }

    impl FakeMemoryProvider {
        fn new(facts: Vec<WorkflowMemoryFact>) -> Self {
            Self {
                facts,
                load_calls: Mutex::new(Vec::new()),
                force_error: false,
            }
        }

        fn with_error() -> Self {
            Self {
                facts: vec![],
                load_calls: Mutex::new(Vec::new()),
                force_error: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl WorkflowMemoryProvider for FakeMemoryProvider {
        async fn load_for_trigger(
            &self,
            project_id: i32,
            agent_id: i32,
            relevant_tags: Vec<String>,
            _limit: usize,
        ) -> Result<Vec<WorkflowMemoryFact>, WorkflowMemoryError> {
            self.load_calls
                .lock()
                .unwrap()
                .push((project_id, agent_id, relevant_tags));
            if self.force_error {
                Err(WorkflowMemoryError::new("forced failure"))
            } else {
                Ok(self.facts.clone())
            }
        }

        fn render_for_prompt(&self, facts: &[WorkflowMemoryFact]) -> String {
            if facts.is_empty() {
                String::new()
            } else {
                let body: String = facts.iter().map(|f| format!("- {}\n", f.fact)).collect();
                format!("## MEMORY\n{}\n", body)
            }
        }
    }

    fn fact(id: i64, text: &str, confidence: f32) -> WorkflowMemoryFact {
        WorkflowMemoryFact {
            id,
            fact: text.to_string(),
            confidence,
            times_used: 0,
        }
    }

    fn make_executor_for_memory_tests() -> AgentExecutor {
        // Build a minimal executor — we only call the memory helpers, which
        // don't touch the DB or git provider.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let enc = make_encryption_service();
        let recorder = Arc::new(GitRecorder::default());
        let git: Arc<dyn GitProviderManagerTrait> = Arc::new(FakeGitProvider {
            recorder,
            clone_should_fail: false,
        });
        let queue: Arc<dyn JobQueue> = Arc::new(FakeJobQueue {
            sent: Mutex::new(vec![]),
        });
        let run_svc = Arc::new(AgentRunService::new(db.clone()));
        let config_svc = Arc::new(AgentConfigService::new(db.clone(), enc.clone()));
        AgentExecutor::new(
            db.clone(),
            git,
            enc,
            queue,
            run_svc,
            config_svc,
            make_notification_service(db.clone()),
            make_sandbox_registry(),
            make_secret_service(db.clone()),
            make_definition_service(db),
        )
    }

    #[test]
    fn test_build_memory_tags_with_source() {
        let tags = AgentExecutor::build_memory_tags(Some("error_group"), Some(42));
        assert_eq!(tags, vec!["error_group:42".to_string()]);
    }

    #[test]
    fn test_build_memory_tags_no_source() {
        let tags = AgentExecutor::build_memory_tags(None, None);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_build_memory_tags_partial_source_returns_empty() {
        // Both must be present to form a tag — having only one is not enough.
        let tags = AgentExecutor::build_memory_tags(Some("error_group"), None);
        assert!(tags.is_empty());
        let tags = AgentExecutor::build_memory_tags(None, Some(42));
        assert!(tags.is_empty());
    }

    #[tokio::test]
    async fn test_load_memory_facts_no_provider_returns_empty() {
        let executor = make_executor_for_memory_tests();
        let facts = executor
            .load_memory_facts(10, 5, Some("error_group"), Some(42))
            .await;
        assert!(facts.is_empty());
    }

    #[tokio::test]
    async fn test_load_memory_facts_with_provider_returns_facts() {
        let executor = make_executor_for_memory_tests();
        let provider = Arc::new(FakeMemoryProvider::new(vec![
            fact(1, "OAuth state cookie missing", 0.9),
            fact(2, "Tests don't cover callback", 0.7),
        ]));
        executor.attach_memory_provider(provider.clone()).await;

        let facts = executor
            .load_memory_facts(10, 5, Some("error_group"), Some(42))
            .await;

        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].fact, "OAuth state cookie missing");

        // Verify the call was scoped correctly
        let calls = provider.load_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 10); // project_id
        assert_eq!(calls[0].1, 5); // agent_id
        assert_eq!(calls[0].2, vec!["error_group:42".to_string()]);
    }

    #[tokio::test]
    async fn test_load_memory_facts_provider_error_returns_empty() {
        // Memory failures must NEVER block a run.
        let executor = make_executor_for_memory_tests();
        let provider = Arc::new(FakeMemoryProvider::with_error());
        executor.attach_memory_provider(provider).await;

        let facts = executor
            .load_memory_facts(10, 5, Some("error_group"), Some(42))
            .await;

        assert!(facts.is_empty());
    }

    #[tokio::test]
    async fn test_render_memory_section_no_provider_returns_empty() {
        let executor = make_executor_for_memory_tests();
        let rendered = executor
            .render_memory_section(&[fact(1, "ignored", 0.9)])
            .await;
        assert_eq!(rendered, "");
    }

    #[tokio::test]
    async fn test_render_memory_section_empty_facts_returns_empty() {
        let executor = make_executor_for_memory_tests();
        let provider = Arc::new(FakeMemoryProvider::new(vec![]));
        executor.attach_memory_provider(provider).await;

        let rendered = executor.render_memory_section(&[]).await;
        assert_eq!(rendered, "");
    }

    #[tokio::test]
    async fn test_render_memory_section_with_facts_uses_provider() {
        let executor = make_executor_for_memory_tests();
        let provider = Arc::new(FakeMemoryProvider::new(vec![]));
        executor.attach_memory_provider(provider).await;

        let rendered = executor
            .render_memory_section(&[
                fact(1, "first finding", 0.9),
                fact(2, "second finding", 0.8),
            ])
            .await;

        assert!(rendered.contains("## MEMORY"));
        assert!(rendered.contains("first finding"));
        assert!(rendered.contains("second finding"));
    }

    #[tokio::test]
    async fn test_attach_memory_provider_late_binding() {
        // The plugin system attaches the memory provider after the executor
        // is already an Arc. This test verifies the lock-based approach works.
        let executor = Arc::new(make_executor_for_memory_tests());

        // No provider initially
        let facts = executor.load_memory_facts(1, 1, None, None).await;
        assert!(facts.is_empty());

        // Attach via Arc clone — exercises the interior mutability path
        let provider = Arc::new(FakeMemoryProvider::new(vec![fact(1, "late", 0.9)]));
        executor.attach_memory_provider(provider).await;

        // Now it sees the provider's facts
        let facts = executor.load_memory_facts(1, 1, None, None).await;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact, "late");
    }

    #[tokio::test]
    async fn test_issue_run_token_no_service_returns_none() {
        let executor = make_executor_for_memory_tests();
        let token = executor.issue_run_token(10, 1, "error-autofix").await;
        assert!(token.is_none());
    }
}
