use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::get,
    Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_entities::{agent_run_logs, agent_runs};

use crate::error::AgentError;
use crate::handlers::AppState;

// ── Response DTOs ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AgentRunResponse {
    pub id: i32,
    pub project_id: i32,
    /// Optional. NULL for ephemeral CLI runs (`source = "cli_ephemeral"`) and
    /// historical autofixer runs that pre-date the agent_id column.
    pub config_id: Option<i32>,
    /// Slug of the agent that created this run, if available.
    pub agent_slug: Option<String>,
    /// Name of the agent that created this run, if available.
    pub agent_name: Option<String>,
    /// `committed` (the run's config lives in `project_agents`) or
    /// `cli_ephemeral` (the config was uploaded via the CLI for a one-off
    /// dry run; see `ephemeral_yaml`).
    pub source: String,
    /// Full WorkflowYamlConfig as YAML text. Populated only when
    /// `source = "cli_ephemeral"`. Used by the web UI to show a "View YAML"
    /// modal so the user can see exactly what the executor ran.
    pub ephemeral_yaml: Option<String>,
    pub trigger_type: String,
    pub trigger_source_id: Option<i32>,
    pub trigger_source_type: Option<String>,
    pub status: String,
    pub branch_name: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub preview_url: Option<String>,
    pub error_message: Option<String>,
    pub ai_output: Option<String>,
    pub ai_reasoning: Option<String>,
    pub ai_model: Option<String>,
    /// AI provider slug that executed this run (e.g. claude_cli, codex_cli, opencode).
    pub ai_provider: Option<String>,
    pub tokens_input: i32,
    pub tokens_output: i32,
    pub estimated_cost_cents: i32,
    pub files_changed: i32,
    /// Report / analysis text produced by the agent (used for report/notification deliverables).
    pub analysis: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    /// Legacy field — all runs now execute in a sandbox. Kept for
    /// backwards-compatible JSON shape; always `true`.
    pub sandbox_enabled: bool,
    /// User-provided context for this run (e.g. webhook payload, manual instructions).
    pub user_context: Option<String>,
    /// Claude CLI session UUID for resuming conversations via `--resume`.
    pub ai_session_id: Option<String>,
    /// Final assembled prompt the AI CLI actually saw (trigger context block +
    /// YAML prompt, with error-group fields interpolated). Captured once per
    /// run. `None` for pre-migration rows.
    pub prompt_text: Option<String>,
    /// Autofixer phase: "analyzing", "analyzed", "fixing", "fix_ready", "no_fix",
    /// "pr_created", or NULL for non-autofixer runs.
    pub phase: Option<String>,
}

impl AgentRunResponse {
    /// Build from a run model, enriched with the agent's slug and name.
    /// `sandbox_enabled` is always emitted as `true` — every run now
    /// executes inside a Docker sandbox.
    pub fn from_with_agent(
        model: agent_runs::Model,
        agent_slug: Option<String>,
        agent_name: Option<String>,
    ) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            config_id: model.config_id,
            agent_slug,
            agent_name,
            source: model.source,
            ephemeral_yaml: model.ephemeral_yaml,
            trigger_type: model.trigger_type,
            trigger_source_id: model.trigger_source_id,
            trigger_source_type: model.trigger_source_type,
            status: model.status,
            branch_name: model.branch_name,
            commit_sha: model.commit_sha,
            pr_url: model.pr_url,
            pr_number: model.pr_number,
            preview_url: model.preview_url,
            error_message: model.error_message,
            ai_output: model.ai_output,
            ai_reasoning: model.ai_reasoning,
            ai_model: model.ai_model,
            ai_provider: model.ai_provider,
            tokens_input: model.tokens_input,
            tokens_output: model.tokens_output,
            estimated_cost_cents: model.estimated_cost_cents,
            files_changed: model.files_changed,
            analysis: model.analysis,
            started_at: model.started_at.map(|t| t.to_rfc3339()),
            completed_at: model.completed_at.map(|t| t.to_rfc3339()),
            created_at: model.created_at.to_rfc3339(),
            sandbox_enabled: true,
            user_context: model.user_context,
            ai_session_id: model.ai_session_id,
            prompt_text: model.prompt_text,
            phase: model.phase,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AgentRunLogResponse {
    pub id: i64,
    pub run_id: i32,
    pub level: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

impl From<agent_run_logs::Model> for AgentRunLogResponse {
    fn from(model: agent_run_logs::Model) -> Self {
        Self {
            id: model.id,
            run_id: model.run_id,
            level: model.level,
            message: model.message,
            metadata: model.metadata,
            created_at: model.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AgentRunWithLogsResponse {
    pub run: AgentRunResponse,
    pub logs: Vec<AgentRunLogResponse>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ListRunsResponse {
    pub items: Vec<AgentRunResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize)]
pub struct ListRunsQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LatestForSourceQuery {
    pub trigger_source_type: String,
    pub trigger_source_id: i32,
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        // All runs for a project (across all agents)
        .route("/projects/{project_id}/agents/runs", get(list_all_runs))
        // Latest run for a given trigger source (e.g. error_group). Indexed
        // lookup so it scales to millions of runs — used by the "Fix with AI"
        // button on error detail pages.
        .route(
            "/projects/{project_id}/agents/runs/latest-for-source",
            get(latest_run_for_source),
        )
        // All runs for a specific agent
        .route(
            "/projects/{project_id}/agents/{slug}/runs",
            get(list_agent_runs),
        )
        // Single run detail by run ID
        .route(
            "/projects/{project_id}/agents/runs/{run_id}",
            get(get_run_with_logs),
        )
        // Cancel a running run
        .route(
            "/projects/{project_id}/agents/runs/{run_id}/cancel",
            axum::routing::post(cancel_run),
        )
        // Retry a completed/failed run with the same context
        .route(
            "/projects/{project_id}/agents/runs/{run_id}/retry",
            axum::routing::post(retry_run),
        )
        // SSE stream for real-time run events
        .route(
            "/projects/{project_id}/agents/runs/{run_id}/stream",
            get(stream_run_events),
        )
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/runs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("page_size" = Option<u64>, Query, description = "Page size (max 100)"),
    ),
    responses(
        (status = 200, description = "List of all agent runs for a project", body = ListRunsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_all_runs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListRunsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);

    let (runs, total) = app_state
        .run_service
        .list_runs(project_id, Some(page), Some(page_size))
        .await
        .map_err(Problem::from)?;

    // Enrich with agent slug/name by looking up the config_id
    let agents = app_state
        .config_service
        .list_agents(project_id)
        .await
        .map_err(Problem::from)?;

    let items = runs
        .into_iter()
        .map(|run| {
            let agent = agents.iter().find(|a| Some(a.id) == run.config_id);
            AgentRunResponse::from_with_agent(
                run,
                agent.map(|a| a.slug.clone()),
                agent.map(|a| a.name.clone()),
            )
        })
        .collect();

    Ok(Json(ListRunsResponse {
        items,
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/runs/latest-for-source",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("trigger_source_type" = String, Query, description = "Trigger source type, e.g. 'error_group'"),
        ("trigger_source_id" = i32, Query, description = "Trigger source ID"),
    ),
    responses(
        (status = 200, description = "Latest matching run, or null if none", body = Option<AgentRunResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn latest_run_for_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<LatestForSourceQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let run = app_state
        .run_service
        .latest_run_for_trigger_source(
            project_id,
            &query.trigger_source_type,
            query.trigger_source_id,
        )
        .await
        .map_err(Problem::from)?;

    let Some(run) = run else {
        return Ok(Json(serde_json::Value::Null));
    };

    // Resolve agent slug/name + sandbox status (mirrors list_all_runs)
    let agents = app_state
        .config_service
        .list_agents(project_id)
        .await
        .map_err(Problem::from)?;

    let agent = agents.iter().find(|a| Some(a.id) == run.config_id);

    let response = AgentRunResponse::from_with_agent(
        run,
        agent.map(|a| a.slug.clone()),
        agent.map(|a| a.name.clone()),
    );

    Ok(Json(
        serde_json::to_value(response).unwrap_or(serde_json::Value::Null),
    ))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/{slug}/runs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Agent slug"),
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("page_size" = Option<u64>, Query, description = "Page size (max 100)"),
    ),
    responses(
        (status = 200, description = "List of runs for a specific agent", body = ListRunsResponse),
        (status = 404, description = "Agent not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_agent_runs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, slug)): Path<(i32, String)>,
    Query(query): Query<ListRunsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let agent = app_state
        .config_service
        .get_agent_by_slug(project_id, &slug)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| {
            Problem::from(crate::error::AgentError::AgentNotFound {
                project_id,
                slug: slug.clone(),
            })
        })?;

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);

    let (runs, total) = app_state
        .run_service
        .list_runs_for_agent(agent.id, Some(page), Some(page_size))
        .await
        .map_err(Problem::from)?;

    let items = runs
        .into_iter()
        .map(|run| {
            AgentRunResponse::from_with_agent(
                run,
                Some(agent.slug.clone()),
                Some(agent.name.clone()),
            )
        })
        .collect();

    Ok(Json(ListRunsResponse {
        items,
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/runs/{run_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 200, description = "Run with logs", body = AgentRunWithLogsResponse),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_run_with_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    let run_with_logs = app_state
        .run_service
        .get_run_with_logs(run_id)
        .await
        .map_err(Problem::from)?;

    // Enrich with agent slug/name. Ephemeral runs have no `project_agents`
    // row, so skip the lookup entirely.
    let agent = match run_with_logs.run.config_id {
        Some(id) => app_state
            .config_service
            .get_agent_by_id(id)
            .await
            .map_err(Problem::from)?,
        None => None,
    };

    let run_resp = AgentRunResponse::from_with_agent(
        run_with_logs.run,
        agent.as_ref().map(|a| a.slug.clone()),
        agent.as_ref().map(|a| a.name.clone()),
    );

    Ok(Json(AgentRunWithLogsResponse {
        run: run_resp,
        logs: run_with_logs
            .logs
            .into_iter()
            .map(AgentRunLogResponse::from)
            .collect(),
    }))
}

/// SSE endpoint for real-time streaming of run events.
/// Polls the agent_run_logs table every 500ms for new entries and streams them.
/// Closes when the run reaches a terminal status.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/agents/runs/{run_id}/stream",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "Server-Sent Events stream of run log events and terminal status", content_type = "text/event-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn stream_run_events(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    let run_service = app_state.run_service.clone();

    let stream = async_stream::stream! {
        let mut last_log_id: i64 = 0;
        let terminal = ["completed", "failed", "no_fix", "cancelled"];

        loop {
            // Fetch new logs since last_log_id
            match run_service.get_logs_after(run_id, last_log_id).await {
                Ok(logs) => {
                    for log in logs {
                        if log.id > last_log_id {
                            last_log_id = log.id;
                        }
                        let data = serde_json::json!({
                            "id": log.id,
                            "level": log.level,
                            "message": log.message,
                            "metadata": log.metadata,
                            "created_at": log.created_at.to_rfc3339(),
                        });
                        yield Ok(Event::default().data(data.to_string()));
                    }
                }
                Err(e) => {
                    tracing::warn!("SSE stream error for run {}: {}", run_id, e);
                }
            }

            // Check if run is in terminal state
            match run_service.get_run(run_id).await {
                Ok(run) => {
                    if terminal.contains(&run.status.as_str()) {
                        // Send final status event and close
                        let status_event = serde_json::json!({
                            "type": "run_status",
                            "status": run.status,
                        });
                        yield Ok(Event::default().event("status").data(status_event.to_string()));
                        break;
                    }
                }
                Err(_) => break,
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    };

    Ok(Sse::new(stream).keep_alive(sse_keep_alive()))
}

/// Shared SSE keep-alive: sends `: heartbeat` comment every 15s so that
/// mobile/proxy-behind clients detect dropped connections quickly. The
/// EventSource API's built-in reconnect fires when no bytes arrive for
/// `retry` milliseconds; a regular heartbeat keeps idle connections
/// flowing and bounds the gap between "network dropped" and "client
/// notices". The text label (vs axum's empty-comment default) also makes
/// the heartbeat visible in browser devtools for debugging.
fn sse_keep_alive() -> KeepAlive {
    KeepAlive::new()
        .interval(std::time::Duration::from_secs(15))
        .text("heartbeat")
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/agents/runs/{run_id}/cancel",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Agent run ID to cancel"),
    ),
    responses(
        (status = 200, description = "Run cancelled", body = AgentRunResponse),
        (status = 400, description = "Run is already in a terminal state"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    let run = app_state
        .run_service
        .cancel_run(run_id)
        .await
        .map_err(Problem::from)?;

    let agents = app_state
        .config_service
        .list_agents(project_id)
        .await
        .map_err(Problem::from)?;
    let agent = agents.iter().find(|a| Some(a.id) == run.config_id);

    Ok(Json(AgentRunResponse::from_with_agent(
        run,
        agent.map(|a| a.slug.clone()),
        agent.map(|a| a.name.clone()),
    )))
}

/// Retry a completed, failed, cancelled, or no_fix run with the same trigger context.
/// Creates a new run record and spawns the executor.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/agents/runs/{run_id}/retry",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID to retry"),
    ),
    responses(
        (status = 202, description = "New run created from retry", body = AgentRunResponse),
        (status = 400, description = "Run is still active"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn retry_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    // Load the original run
    let original = app_state
        .run_service
        .get_run(run_id)
        .await
        .map_err(Problem::from)?;

    // Only allow retry on terminal runs
    let terminal_statuses = ["completed", "failed", "no_fix", "cancelled"];
    if !terminal_statuses.contains(&original.status.as_str()) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Run Still Active")
            .with_detail(format!(
                "Cannot retry run {} — it is still in '{}' status. Cancel it first.",
                run_id, original.status
            )));
    }

    // Ephemeral CLI runs can't be retried server-side: the YAML lives only on
    // the original run row, the user already has it locally, and re-running
    // it via this endpoint would let anyone with permission to read the run
    // fork an arbitrary workflow. Tell them to re-trigger from the CLI.
    if original.source == "cli_ephemeral" {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Cannot Retry Ephemeral Run")
            .with_detail(format!(
                "Run {} was triggered via `temps workflow run --from-file`. \
                 Re-run it from your machine instead.",
                run_id
            )));
    }

    let original_config_id = original.config_id.ok_or_else(|| {
        Problem::from(AgentError::Validation {
            message: format!("Run {} has no config_id and cannot be retried.", run_id),
        })
    })?;

    // Verify the agent still exists
    let agent = app_state
        .config_service
        .get_agent_by_id(original_config_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| {
            Problem::from(AgentError::Validation {
                message: format!(
                    "The workflow for run {} no longer exists (config_id={}).",
                    run_id, original_config_id
                ),
            })
        })?;

    // Create a new run with the same trigger context
    let new_run = app_state
        .run_service
        .create_run(
            original.project_id,
            original_config_id,
            "retry".to_string(),
            original.trigger_source_id,
            original.trigger_source_type,
            original.user_context,
        )
        .await
        .map_err(Problem::from)?;

    // Spawn the executor
    let executor = app_state.executor.clone();
    let new_run_id = new_run.id;
    tokio::spawn(async move {
        executor.execute_run(new_run_id).await;
    });

    let run_resp = AgentRunResponse::from_with_agent(new_run, Some(agent.slug), Some(agent.name));

    Ok((StatusCode::ACCEPTED, Json(run_resp)))
}
