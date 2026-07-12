use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;

use crate::ai_cli::{self, AiCliStatus};
use crate::error::AgentError;
use crate::handlers::runs::AgentRunResponse;
use crate::handlers::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct TriggerAgentRequest {
    pub trigger_source_type: Option<String>,
    pub trigger_source_id: Option<i32>,
    /// Optional context from the user (e.g. a research topic, bug description, or instructions).
    pub user_context: Option<String>,
}

// ── Audit ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct AgentRunTriggeredAudit {
    context: AuditContext,
    project_id: i32,
    agent_slug: String,
    run_id: i32,
    trigger_type: String,
}

impl AuditOperation for AgentRunTriggeredAudit {
    fn operation_type(&self) -> String {
        "AGENT_RUN_TRIGGERED".to_string()
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| temps_core::anyhow::anyhow!("Failed to serialize audit: {}", e))
    }
}

// ── Routes ────────────────────────────────────────────────────────────────────

// Note: the ephemeral CLI dry-run endpoint (`/projects/{id}/workflows/dry-run`)
// lives in `handlers::workflows` — it owns its own routes, request type, and
// audit struct.

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/agents/{slug}/trigger",
            post(trigger_agent),
        )
        // Public webhook endpoint — X-Webhook-Token header auth
        .route("/agents/webhook/{webhook_id}", post(webhook_trigger))
        .route(
            "/projects/{project_id}/agents/cli-status",
            get(get_cli_status),
        )
        .route(
            "/projects/{project_id}/agents/sandbox-status",
            get(get_sandbox_status),
        )
        // Global sandbox status and management (for settings page)
        .route("/settings/sandbox-status", get(get_global_sandbox_status))
        .route("/settings/sandbox-rebuild", post(rebuild_sandbox_image))
        .route(
            "/projects/{project_id}/agents/smoke-test",
            post(smoke_test_agent),
        )
        .route("/settings/agent-token", post(save_agent_token))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/agents/{slug}/trigger",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Agent slug"),
    ),
    request_body = TriggerAgentRequest,
    responses(
        (status = 202, description = "Agent run created and queued", body = AgentRunResponse),
        (status = 400, description = "Validation error"),
        (status = 402, description = "Daily budget exceeded"),
        (status = 404, description = "Agent not found"),
        (status = 422, description = "AI CLI not installed"),
        (status = 429, description = "Cooldown active"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn trigger_agent(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, slug)): Path<(i32, String)>,
    Json(request): Json<TriggerAgentRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    // Load agent to get config_id and verify it is enabled
    let agent = app_state
        .config_service
        .get_agent_by_slug(project_id, &slug)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| {
            Problem::from(AgentError::AgentNotFound {
                project_id,
                slug: slug.clone(),
            })
        })?;

    if !agent.enabled {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Agent '{}' is disabled for project {}. Enable it in the config first.",
                slug, project_id
            ),
        }));
    }

    // If the caller is linking the run to an error group, validate it here so
    // they get a 400 synchronously. The executor also revalidates (and checks
    // cross-project ownership) in `load_error_context`, but failing early
    // surfaces the problem immediately in the HTTP response instead of in a
    // background run that errors out mid-execution.
    if request.trigger_source_type.as_deref() == Some("error_group") {
        use sea_orm::EntityTrait;
        let group_id = request.trigger_source_id.ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: "trigger_source_id is required when trigger_source_type is 'error_group'"
                    .to_string(),
            })
        })?;
        let group = temps_entities::error_groups::Entity::find_by_id(group_id)
            .one(app_state.db.as_ref())
            .await
            .map_err(AgentError::Database)
            .map_err(Problem::from)?
            .ok_or_else(|| {
                Problem::from(AgentError::Validation {
                    message: format!(
                        "Error group {} not found for project {}",
                        group_id, project_id
                    ),
                })
            })?;
        if group.project_id != project_id {
            return Err(Problem::from(AgentError::Validation {
                message: format!(
                    "Error group {} does not belong to project {}",
                    group_id, project_id
                ),
            }));
        }
    }

    // Create the run record with "pending" status
    let run = app_state
        .run_service
        .create_run(
            project_id,
            agent.id,
            "manual".to_string(),
            request.trigger_source_id,
            request.trigger_source_type.clone(),
            request.user_context,
        )
        .await
        .map_err(Problem::from)?;

    // Spawn the executor in the background
    let executor = app_state.executor.clone();
    let run_id = run.id;
    tokio::spawn(async move {
        executor.execute_run(run_id).await;
    });

    // Audit log (non-fatal)
    let audit = AgentRunTriggeredAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        agent_slug: slug.clone(),
        run_id: run.id,
        trigger_type: "manual".to_string(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for agent trigger (project {}, slug {}, run {}): {}",
            project_id,
            slug,
            run.id,
            e
        );
    }

    let run_resp = AgentRunResponse::from_with_agent(run, Some(agent.slug), Some(agent.name));

    Ok((StatusCode::ACCEPTED, Json(run_resp)))
}

// ── Public Webhook Trigger ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct WebhookTriggerRequest {
    /// Arbitrary JSON payload from the caller. Passed to the agent as user_context.
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookTriggerResponse {
    pub run_id: i32,
    pub status: String,
}

/// Constant-time byte comparison to prevent timing attacks on webhook tokens.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Public webhook endpoint. Authenticated via `X-Webhook-Token` header.
///
/// `POST /api/agents/webhook/{webhook_id}`
/// Header: `X-Webhook-Token: <secret>`
///
/// The `webhook_id` in the URL is a short non-secret identifier (safe to log).
/// The actual credential is the secret token in the header.
///
/// Accepts any JSON body, which is passed as `user_context` to the agent run.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/agents/webhook/{webhook_id}",
    params(
        ("webhook_id" = String, Path, description = "Webhook ID (non-secret)"),
    ),
    request_body = WebhookTriggerRequest,
    responses(
        (status = 202, description = "Agent run created", body = WebhookTriggerResponse),
        (status = 401, description = "Missing or invalid X-Webhook-Token header"),
        (status = 404, description = "Invalid webhook ID"),
        (status = 422, description = "Agent disabled"),
    ),
)]
pub async fn webhook_trigger(
    State(app_state): State<Arc<AppState>>,
    Path(webhook_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WebhookTriggerRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Extract X-Webhook-Token header
    let provided_token = headers
        .get("x-webhook-token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("Missing Webhook Token")
                .with_detail("X-Webhook-Token header is required")
        })?;

    // Look up agent by webhook ID (non-secret URL identifier)
    let agent = app_state
        .config_service
        .get_agent_by_webhook_id(&webhook_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| {
            problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Invalid Webhook ID")
                .with_detail("No agent found for the provided webhook ID")
        })?;

    // Validate the secret token from the header
    let expected_token = agent.webhook_token.as_deref().ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Webhook Not Configured")
            .with_detail("This agent does not have webhook triggers enabled")
    })?;

    if !constant_time_eq(provided_token.as_bytes(), expected_token.as_bytes()) {
        return Err(problemdetails::new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Webhook Token")
            .with_detail("The provided X-Webhook-Token does not match"));
    }

    if !agent.enabled {
        return Err(problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
            .with_title("Agent Disabled")
            .with_detail(format!(
                "Agent '{}' is disabled. Enable it before triggering via webhook.",
                agent.slug
            )));
    }

    // Serialize the payload as user_context
    let user_context =
        if request.payload.is_null() || request.payload.as_object().is_some_and(|m| m.is_empty()) {
            None
        } else {
            Some(serde_json::to_string(&request.payload).unwrap_or_default())
        };

    // Create the run
    let run = app_state
        .run_service
        .create_run(
            agent.project_id,
            agent.id,
            "webhook".to_string(),
            None,
            Some("webhook".to_string()),
            user_context,
        )
        .await
        .map_err(Problem::from)?;

    // Spawn the executor
    let executor = app_state.executor.clone();
    let run_id = run.id;
    tokio::spawn(async move {
        executor.execute_run(run_id).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(WebhookTriggerResponse {
            run_id: run.id,
            status: "pending".to_string(),
        }),
    ))
}

// ── CLI Status ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CliStatusQuery {
    /// AI provider to check: "claude_cli" or "codex_cli". Defaults to "claude_cli".
    pub provider: Option<String>,
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/cli-status",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("provider" = Option<String>, Query, description = "AI provider: claude_cli or codex_cli"),
    ),
    responses(
        (status = 200, description = "CLI status"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cli_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<CliStatusQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let provider_name = query.provider.as_deref().unwrap_or("claude_cli");

    let status = match ai_cli::create_provider(provider_name) {
        Some(provider) => provider.get_status().await,
        None => AiCliStatus {
            provider: provider_name.into(),
            installed: false,
            version: None,
            authenticated: false,
            auth_method: None,
            email: None,
            subscription_type: None,
            setup_hint: Some(format!(
                "Unknown AI provider '{}'. Supported: claude_cli, opencode, codex_cli",
                provider_name
            )),
        },
    };

    Ok(Json(status))
}

// ── Sandbox Status ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct SandboxStatusResponse {
    pub docker_available: bool,
    pub image_ready: bool,
    pub image_name: String,
    pub error: Option<String>,
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/sandbox-status",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Project-scoped sandbox readiness (Docker + agent image)", body = SandboxStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_sandbox_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let sandbox_registry = &app_state.executor.sandbox_registry();
    let provider = sandbox_registry.provider();

    // Check Docker connectivity
    let docker_available = provider.is_available().await;

    // Check if sandbox image exists
    let (image_ready, image_name, error) = if docker_available {
        match provider.image_status().await {
            Ok((ready, name)) => (ready, name, None),
            Err(e) => (false, String::new(), Some(e.to_string())),
        }
    } else {
        (
            false,
            String::new(),
            Some("Docker is not available on this server".to_string()),
        )
    };

    Ok(Json(SandboxStatusResponse {
        docker_available,
        image_ready,
        image_name,
        error,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/sandbox-status",
    responses(
        (status = 200, description = "Global sandbox readiness for the settings page", body = SandboxStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_sandbox_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let provider = app_state.executor.sandbox_registry().provider();
    let docker_available = provider.is_available().await;

    let (image_ready, image_name, error) = if docker_available {
        match provider.image_status().await {
            Ok((ready, name)) => (ready, name, None),
            Err(e) => (false, String::new(), Some(e.to_string())),
        }
    } else {
        (
            false,
            String::new(),
            Some("Docker is not available on this server".to_string()),
        )
    };

    Ok(Json(SandboxStatusResponse {
        docker_available,
        image_ready,
        image_name,
        error,
    }))
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/sandbox-rebuild",
    responses(
        (status = 200, description = "Server-Sent Events stream of rebuild progress; final event `{\"type\":\"done\",\"success\":bool,...}`", content_type = "text/event-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn rebuild_sandbox_image(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, Problem> {
    permission_guard!(auth, SettingsWrite);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);

    // Spawn the build in the background, sending progress to the channel.
    let provider = app_state.executor.sandbox_registry().provider_arc();
    tokio::spawn(async move {
        let result = provider.rebuild_image_with_progress(tx.clone()).await;
        match result {
            Ok(name) => {
                let _ = tx
                    .send(
                        serde_json::json!({
                            "type": "done",
                            "success": true,
                            "image_name": name,
                        })
                        .to_string(),
                    )
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(
                        serde_json::json!({
                            "type": "done",
                            "success": false,
                            "error": e.to_string(),
                        })
                        .to_string(),
                    )
                    .await;
            }
        }
    });

    let stream = async_stream::stream! {
        while let Some(msg) = rx.recv().await {
            yield Ok(Event::default().data(msg));
        }
    };

    Ok(Sse::new(stream).keep_alive(sse_keep_alive()))
}

/// Shared SSE keep-alive: sends `: heartbeat` every 15s so clients behind
/// slow networks or idle proxies detect dropped connections quickly. See
/// `handlers::runs::sse_keep_alive` for the full rationale.
fn sse_keep_alive() -> KeepAlive {
    KeepAlive::new()
        .interval(std::time::Duration::from_secs(15))
        .text("heartbeat")
}

// ── Smoke Test ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct SmokeTestResponse {
    /// Whether the smoke test passed
    pub passed: bool,
    /// Where the test ran: "host" or "sandbox"
    pub environment: String,
    /// Claude CLI installed?
    pub cli_installed: bool,
    /// Claude CLI authenticated?
    pub cli_authenticated: bool,
    /// Claude CLI version
    pub cli_version: Option<String>,
    /// Auth email / method
    pub auth_info: Option<String>,
    /// What the user needs to do if the test failed
    pub setup_hint: Option<String>,
    /// Full output for debugging
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SmokeTestQuery {
    /// Provider id to test. Defaults to the globally active provider when
    /// omitted so existing UI/CLI callers keep working. Per-provider Test
    /// buttons pass the explicit provider id so users can verify any
    /// credential, not just the one currently marked active.
    pub provider_id: Option<String>,
}

/// Run a smoke test to verify the selected AI CLI works in the environment
/// where agents will actually execute (host or sandbox container). If no
/// `provider_id` is supplied the globally active provider is tested.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/agents/smoke-test",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("provider_id" = Option<String>, Query, description = "Provider id to test; defaults to the globally active provider"),
    ),
    responses(
        (status = 200, description = "Smoke test result for the AI CLI in the agent's execution environment", body = SmokeTestResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn smoke_test_agent(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<SmokeTestQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    // Check if sandbox is enabled globally
    let global_sandbox = {
        use sea_orm::EntityTrait;
        temps_entities::settings::Entity::find_by_id(1)
            .one(app_state.db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|s| {
                s.data.get("agent_sandbox").cloned().and_then(|v| {
                    serde_json::from_value::<temps_core::AgentSandboxSettings>(v).ok()
                })
            })
            .unwrap_or_default()
    };

    // Resolve which provider to test. Fall back to the globally active one
    // when the caller didn't pass ?provider_id=…
    let target_provider_id = query
        .provider_id
        .unwrap_or_else(|| global_sandbox.default_provider.clone());

    // Reject unknown provider ids — matches how activate/credential endpoints
    // behave and keeps error messages precise.
    let Some(catalog_entry) = ai_cli::catalog::find_provider(&target_provider_id) else {
        return Ok(Json(SmokeTestResponse {
            passed: false,
            environment: "host".into(),
            cli_installed: false,
            cli_authenticated: false,
            cli_version: None,
            auth_info: None,
            setup_hint: Some(format!(
                "Unknown provider id '{}'. Valid ids: claude_cli, codex_cli, opencode.",
                target_provider_id
            )),
            detail: None,
        }));
    };

    // Pull the saved credential + auth flavor for this specific provider.
    // `provider_config` handles the legacy flat-field fallback for claude_cli.
    let provider_cfg = global_sandbox.provider_config(&target_provider_id);
    let auth_flavor = catalog_entry
        .flavor(&provider_cfg.auth_type)
        .unwrap_or_else(|| catalog_entry.default_flavor());

    {
        // Sandbox is the only execution mode. Test inside a sandbox container.
        let registry = app_state.executor.sandbox_registry();
        let provider = registry.provider();

        if !provider.is_available().await {
            return Ok(Json(SmokeTestResponse {
                passed: false,
                environment: "sandbox".into(),
                cli_installed: false,
                cli_authenticated: false,
                cli_version: None,
                auth_info: None,
                setup_hint: Some(
                    "Docker is not available. Install Docker or disable sandbox mode.".into(),
                ),
                detail: None,
            }));
        }

        // Create a temporary sandbox for the smoke test. Use a provider-scoped
        // run id so concurrent Test clicks on different cards don't collide on
        // the same sandbox.
        let test_run_id: i32 = match target_provider_id.as_str() {
            "claude_cli" => 99_999,
            "codex_cli" => 99_998,
            "opencode" => 99_997,
            _ => 99_996,
        };
        let work_dir =
            std::env::temp_dir().join(format!("agent-smoke-test-{}", target_provider_id));
        let _ = tokio::fs::create_dir_all(&work_dir).await;

        // Inject the saved credential for *this* provider (not the active one)
        // so the smoke test reflects the real auth state of whichever card
        // the user clicked Test on. For `ApiKey` flavors we set the catalog's
        // env var directly; for OAuth/ConfigFile flavors we fall back to the
        // legacy Claude `CLAUDE_CODE_OAUTH_TOKEN` path for claude_cli (older
        // sandboxes still read it) and leave other file-based flavors to the
        // seed-path logic the real session uses. The smoke test's CLI binary
        // will then be able to find its credentials on disk (via the sandbox
        // image's baked-in seed) or via the env var we just set.
        let mut test_env = std::collections::HashMap::new();
        if let Some(ref encrypted) = provider_cfg.credentials_encrypted {
            if let Ok(plain) = app_state.encryption_service.decrypt_string(encrypted) {
                use ai_cli::catalog::CredentialFormat;
                match auth_flavor.format {
                    CredentialFormat::ApiKey if !auth_flavor.env_var.is_empty() => {
                        test_env.insert(auth_flavor.env_var.to_string(), plain);
                    }
                    CredentialFormat::OauthToken if target_provider_id == "claude_cli" => {
                        // Legacy env var still recognized by the Claude CLI.
                        test_env.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), plain);
                    }
                    _ => {
                        // ConfigFile flavors (opencode, codex subscription): the
                        // sandbox image's seed path is what the CLI reads — we
                        // can't materialize that here without the full session
                        // manager. The CLI's auth check will therefore report
                        // "not authenticated" even if the credential is saved.
                        // The setup hint below tells the user to run an actual
                        // session to verify file-based flavors.
                    }
                }
            }
        }

        let image = format!("temps-sandbox-{}:latest", global_sandbox.runtime);
        let sandbox_config = crate::sandbox::SandboxCreateConfig {
            run_id: test_run_id,
            container_name_override: None,
            host_work_dir: work_dir.clone(),
            workspace_volume: None,
            image: Some(image),
            cpu_limit: Some(1.0),
            memory_limit_mb: Some(512),
            pids_limit: None,
            // Use the default egress-filtered bridge network (same as production
            // sandboxes).  The old "host" override was a security hole: it gave
            // the smoke-test container unrestricted access to all host-network
            // services including localhost:8080 (control plane), the DB port,
            // and cloud-metadata endpoints.  Control-plane connectivity checks
            // must be done from outside the container by polling agent_run
            // status from the test harness, not from inside via localhost.
            network_mode: None,
            env_vars: test_env,
            idle_timeout: std::time::Duration::from_secs(60),
        };

        let _handle = match registry.get_or_create(sandbox_config).await {
            Ok(h) => h,
            Err(e) => {
                return Ok(Json(SmokeTestResponse {
                    passed: false,
                    environment: "sandbox".into(),
                    cli_installed: false,
                    cli_authenticated: false,
                    cli_version: None,
                    auth_info: None,
                    setup_hint: Some(format!(
                        "Failed to create sandbox: {}. Try rebuilding the image.",
                        e
                    )),
                    detail: None,
                }));
            }
        };

        // Run a provider-appropriate auth check inside the sandbox. Each CLI
        // has a different command — claude has `claude auth status --json`,
        // codex has `codex auth status`, opencode has `opencode auth list`.
        let check_cmd: Vec<String> = match target_provider_id.as_str() {
            "claude_cli" => vec!["claude".into(), "auth".into(), "status".into()],
            "codex_cli" => vec!["codex".into(), "--version".into()],
            "opencode" => vec!["opencode".into(), "--version".into()],
            _ => vec!["true".into()],
        };
        let result = registry
            .exec(
                test_run_id,
                check_cmd,
                std::collections::HashMap::new(),
                None,
            )
            .await;

        // Clean up
        let _ = registry.release(test_run_id).await;
        let _ = tokio::fs::remove_dir_all(&work_dir).await;

        match result {
            Ok(exec_result) => {
                let output = exec_result.stdout.trim().to_string();
                let cli_installed = exec_result.exit_code != 127; // 127 = command not found

                // Only Claude's `auth status --json` response is rich enough to
                // parse. For the other providers the --version call just tells
                // us the CLI is present; we infer "authenticated" from the saved
                // credential existing (the session manager will seed it at run
                // time).
                let (authenticated, version, auth_info) = if target_provider_id == "claude_cli" {
                    parse_auth_status(&output)
                } else {
                    (
                        provider_cfg.credentials_encrypted.is_some(),
                        Some(output.lines().next().unwrap_or("").trim().to_string())
                            .filter(|s| !s.is_empty()),
                        None,
                    )
                };

                let setup_hint = if !cli_installed {
                    Some(format!(
                        "{} CLI is not installed in the sandbox. Rebuild the sandbox image to pick up the latest provider bundle.",
                        catalog_entry.name
                    ))
                } else if !authenticated {
                    Some(format!(
                        "{} credential isn't saved. Paste your credential above and hit Save.",
                        catalog_entry.name
                    ))
                } else {
                    None
                };

                Ok(Json(SmokeTestResponse {
                    passed: cli_installed && authenticated,
                    environment: "sandbox".into(),
                    cli_installed,
                    cli_authenticated: authenticated,
                    cli_version: version,
                    auth_info,
                    setup_hint,
                    detail: Some(output),
                }))
            }
            Err(e) => Ok(Json(SmokeTestResponse {
                passed: false,
                environment: "sandbox".into(),
                cli_installed: false,
                cli_authenticated: false,
                cli_version: None,
                auth_info: None,
                setup_hint: Some(format!("Smoke test failed: {}", e)),
                detail: None,
            })),
        }
    }
}

fn parse_auth_status(output: &str) -> (bool, Option<String>, Option<String>) {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        let authenticated = json
            .get("loggedIn")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let email = json.get("email").and_then(|v| v.as_str()).map(String::from);
        let version = json
            .get("version")
            .and_then(|v| v.as_str())
            .map(String::from);
        (authenticated, version, email)
    } else {
        // Fallback: check for "Logged in" in plain text
        let authenticated = output.contains("Logged in") || output.contains("loggedIn");
        (authenticated, None, None)
    }
}

// ── Agent Token ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveAgentTokenRequest {
    /// The OAuth token from `claude setup-token` or an API key.
    /// Will be encrypted before storage.
    pub token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SaveAgentTokenResponse {
    pub saved: bool,
}

/// Save an encrypted AI provider token for use in sandbox containers.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/agent-token",
    request_body = SaveAgentTokenRequest,
    responses(
        (status = 200, description = "Token encrypted and persisted", body = SaveAgentTokenResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Encryption or database error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn save_agent_token(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Json(request): Json<SaveAgentTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    use sea_orm::EntityTrait;

    // Encrypt the token
    let encrypted = app_state
        .encryption_service
        .encrypt_string(&request.token)
        .map_err(|e| {
            Problem::from(AgentError::EncryptionError {
                message: format!("Failed to encrypt token: {}", e),
            })
        })?;

    // Load current settings
    let record = temps_entities::settings::Entity::find_by_id(1)
        .one(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    let mut settings_data = record
        .map(|r| r.data)
        .unwrap_or_else(|| serde_json::json!({}));

    // Update the agent_sandbox.api_key_encrypted field
    if let Some(sandbox) = settings_data.get_mut("agent_sandbox") {
        sandbox["api_key_encrypted"] = serde_json::Value::String(encrypted);
    } else {
        settings_data["agent_sandbox"] = serde_json::json!({ "api_key_encrypted": encrypted });
    }

    // Save back
    use sea_orm::{ActiveModelTrait, Set};
    let active = temps_entities::settings::ActiveModel {
        id: Set(1),
        data: Set(settings_data),
        ..Default::default()
    };
    active
        .update(app_state.db.as_ref())
        .await
        .map_err(|e| Problem::from(AgentError::Database(e)))?;

    Ok(Json(SaveAgentTokenResponse { saved: true }))
}

#[cfg(test)]
mod webhook_token_tests {
    use super::constant_time_eq;

    #[test]
    fn accepts_matching_tokens() {
        assert!(constant_time_eq(b"whsec_abc123", b"whsec_abc123"));
    }

    #[test]
    fn rejects_mismatched_tokens_of_equal_length() {
        // Regression test: an earlier version of this check used
        // `!fold(..) == 0`, which parses as `(!fold(..)) == 0` due to Rust's
        // precedence rules (`!` on a `u8` is bitwise NOT, not logical NOT).
        // That accepted every wrong token whose XOR-accumulation wasn't
        // exactly 255, i.e. almost all wrong tokens. Every case below must
        // be rejected.
        assert!(!constant_time_eq(b"whsec_abc123", b"whsec_abc124"));
        assert!(!constant_time_eq(b"whsec_abc123", b"whsec_xyz123"));
        assert!(!constant_time_eq(b"aaaaaaaaaaaa", b"bbbbbbbbbbbb"));
        assert!(!constant_time_eq(b"\0\0\0\0", b"\x01\x01\x01\x01"));
    }

    #[test]
    fn rejects_different_length_tokens() {
        assert!(!constant_time_eq(b"short", b"much_longer_token"));
        assert!(!constant_time_eq(b"", b"nonempty"));
    }

    #[test]
    fn empty_tokens_are_equal() {
        assert!(constant_time_eq(b"", b""));
    }
}

// `list_available_models` was retired in favour of the per-provider
// `models` field on the `/settings/ai-providers` catalog response. Each
// provider declares its own valid model ids in `ai_cli::catalog`, which
// keeps Claude/Codex/OpenCode from sharing one global list (and from
// shelling out to a host-side `opencode` binary that often isn't there).
