use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use temps_entities::agent_runs;

use crate::error::AgentError;
use crate::handlers::runs::AgentRunLogResponse;
use crate::handlers::AppState;

// ── Request DTOs ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartAnalysisRequest {
    pub error_group_id: i32,
    pub user_context: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AddContextRequest {
    pub message: String,
}

// ── Response DTOs ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AutofixerRunResponse {
    pub id: i32,
    pub project_id: i32,
    pub trigger_source_id: Option<i32>,
    pub status: String,
    pub phase: Option<String>,
    pub analysis: Option<String>,
    pub user_context: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<i32>,
    pub branch_name: Option<String>,
    pub error_message: Option<String>,
    pub ai_output: Option<String>,
    pub ai_model: Option<String>,
    pub tokens_input: i32,
    pub tokens_output: i32,
    pub files_changed: i32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
}

impl From<agent_runs::Model> for AutofixerRunResponse {
    fn from(model: agent_runs::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            trigger_source_id: model.trigger_source_id,
            status: model.status,
            phase: model.phase,
            analysis: model.analysis,
            user_context: model.user_context,
            pr_url: model.pr_url,
            pr_number: model.pr_number,
            branch_name: model.branch_name,
            error_message: model.error_message,
            ai_output: model.ai_output,
            ai_model: model.ai_model,
            tokens_input: model.tokens_input,
            tokens_output: model.tokens_output,
            files_changed: model.files_changed,
            started_at: model.started_at.map(|t| t.to_rfc3339()),
            completed_at: model.completed_at.map(|t| t.to_rfc3339()),
            created_at: model.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AutofixerRunWithLogsResponse {
    pub run: AutofixerRunResponse,
    pub logs: Vec<AgentRunLogResponse>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatePrResponse {
    pub run: AutofixerRunResponse,
    pub pr_url: String,
    pub pr_number: i32,
    pub branch_name: String,
}

// ── Audit types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct AutofixerStartAudit {
    context: AuditContext,
    project_id: i32,
    error_group_id: i32,
    run_id: i32,
}

impl AuditOperation for AutofixerStartAudit {
    fn operation_type(&self) -> String {
        "AUTOFIXER_ANALYSIS_STARTED".to_string()
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

#[derive(Debug, Clone, Serialize)]
struct AutofixerFixAudit {
    context: AuditContext,
    project_id: i32,
    run_id: i32,
}

impl AuditOperation for AutofixerFixAudit {
    fn operation_type(&self) -> String {
        "AUTOFIXER_FIX_STARTED".to_string()
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

#[derive(Debug, Clone, Serialize)]
struct AutofixerPrAudit {
    context: AuditContext,
    project_id: i32,
    run_id: i32,
    pr_url: String,
}

impl AuditOperation for AutofixerPrAudit {
    fn operation_type(&self) -> String {
        "AUTOFIXER_PR_CREATED".to_string()
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

// ── Routes ─────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/autofixer/analyze",
            post(start_analysis),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}",
            get(get_run),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/stream",
            get(stream_events),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/add-context",
            post(add_context),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/re-analyze",
            post(re_analyze),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/fix",
            post(start_fix),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/create-pr",
            post(create_pr),
        )
        .route(
            "/projects/{project_id}/autofixer/runs/{run_id}/cancel",
            post(cancel),
        )
}

// ── Handlers ───────────────────────────────────────────────────────────────────

/// Start an autofixer analysis run for the given error group.
/// Creates the run record immediately and spawns analysis in the background.
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/analyze",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = StartAnalysisRequest,
    responses(
        (status = 202, description = "Analysis started; returns run_id for streaming", body = AutofixerRunResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_analysis(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<StartAnalysisRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    if request.error_group_id <= 0 {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Invalid error_group_id {} for project {}",
                request.error_group_id, project_id
            ),
        }));
    }

    // Create run record
    let run = app_state
        .run_service
        .create_autofixer_run(project_id, request.error_group_id, request.user_context)
        .await
        .map_err(Problem::from)?;

    let run_id = run.id;

    // Audit log
    let audit = AutofixerStartAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        error_group_id: request.error_group_id,
        run_id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for autofixer analysis start \
             (project {}, error_group {}, run {}): {}",
            project_id,
            request.error_group_id,
            run_id,
            e
        );
    }

    // Spawn analysis in background
    let autofixer = app_state.autofixer_service.clone();
    tokio::spawn(async move {
        autofixer.run_analysis(run_id).await;
    });

    Ok((StatusCode::ACCEPTED, Json(AutofixerRunResponse::from(run))))
}

/// Get a single autofixer run with its logs.
#[utoipa::path(
    tag = "Autofixer",
    get,
    path = "/projects/{project_id}/autofixer/runs/{run_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 200, description = "Run with logs", body = AutofixerRunWithLogsResponse),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);

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

    Ok(Json(AutofixerRunWithLogsResponse {
        run: AutofixerRunResponse::from(run_with_logs.run),
        logs: run_with_logs
            .logs
            .into_iter()
            .map(AgentRunLogResponse::from)
            .collect(),
    }))
}

/// SSE endpoint: streams run log events in real-time.
/// Polls every 500 ms. Keeps the connection open through "analyzed" and "fix_ready"
/// waiting states; closes only on terminal statuses.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/stream",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Autofixer run ID"),
    ),
    responses(
        (status = 200, description = "Server-Sent Events stream of autofixer run logs and status updates", content_type = "text/event-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Run not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn stream_events(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, Problem> {
    permission_guard!(auth, ProjectsRead);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    let run_service = app_state.run_service.clone();

    let stream = async_stream::stream! {
        let mut last_log_id: i64 = 0;
        // Terminal statuses that should close the stream
        let terminal = ["completed", "failed", "no_fix", "cancelled"];
        // Waiting statuses — keep connection open but stop busy-polling
        let waiting = ["analyzed", "fix_ready"];

        loop {
            // Fetch new log entries
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
                    tracing::warn!("Autofixer SSE stream error for run {}: {}", run_id, e);
                }
            }

            // Check current run status
            match run_service.get_run(run_id).await {
                Ok(run) => {
                    let status = run.status.as_str();
                    if terminal.contains(&status) {
                        let status_event = serde_json::json!({
                            "type": "run_status",
                            "status": run.status,
                            "phase": run.phase,
                        });
                        yield Ok(Event::default().event("status").data(status_event.to_string()));
                        break;
                    }

                    if waiting.contains(&status) {
                        // Emit a heartbeat with the current phase so the frontend can update UI,
                        // then poll less aggressively (2 s instead of 500 ms).
                        let phase_event = serde_json::json!({
                            "type": "run_status",
                            "status": run.status,
                            "phase": run.phase,
                        });
                        yield Ok(Event::default().event("status").data(phase_event.to_string()));
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    }
                }
                Err(_) => break,
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
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

/// Append a user message to the run's context field.
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/add-context",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    request_body = AddContextRequest,
    responses(
        (status = 200, description = "Context appended"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn add_context(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
    Json(request): Json<AddContextRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    if request.message.trim().is_empty() {
        return Err(Problem::from(AgentError::Validation {
            message: format!("Context message for run {} cannot be empty", run_id),
        }));
    }

    app_state
        .autofixer_service
        .add_context(run_id, request.message)
        .await
        .map_err(Problem::from)?;

    Ok(StatusCode::OK)
}

/// Transition from analysis to fix phase.
/// Requires phase == "analyzed".
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/fix",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 202, description = "Fix generation started"),
        (status = 400, description = "Run not in analyzed phase"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn start_fix(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    // Verify the run exists and is in the right phase before spawning
    let run = app_state
        .run_service
        .get_run(run_id)
        .await
        .map_err(Problem::from)?;

    if run.phase.as_deref() != Some("analyzed") {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Run {} cannot start fix: expected phase 'analyzed', got '{}'",
                run_id,
                run.phase.as_deref().unwrap_or("none")
            ),
        }));
    }

    // Audit log
    let audit = AutofixerFixAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        run_id,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for autofixer fix start (project {}, run {}): {}",
            project_id,
            run_id,
            e
        );
    }

    app_state.telemetry.report(
        temps_core::TelemetryEvent::new(temps_core::TelemetryEventKind::AutofixerFixAccepted)
            .with("source", run.source.clone())
            .with_opt("ai_provider", run.ai_provider.clone()),
    );

    // Spawn fix in background
    let autofixer = app_state.autofixer_service.clone();
    tokio::spawn(async move {
        autofixer.run_fix(run_id).await;
    });

    Ok((StatusCode::ACCEPTED, Json(AutofixerRunResponse::from(run))))
}

/// Push the fix branch and create a pull request.
/// Requires phase == "fix_ready".
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/create-pr",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 201, description = "PR created", body = CreatePrResponse),
        (status = 400, description = "Run not in fix_ready phase"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_pr(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    let pr = app_state
        .autofixer_service
        .create_pr(run_id)
        .await
        .map_err(Problem::from)?;

    // Reload run to get updated fields
    let updated_run = app_state
        .run_service
        .get_run(run_id)
        .await
        .map_err(Problem::from)?;

    // Audit log
    let audit = AutofixerPrAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        run_id,
        pr_url: pr.url.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to create audit log for autofixer PR creation (project {}, run {}): {}",
            project_id,
            run_id,
            e
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(CreatePrResponse {
            run: AutofixerRunResponse::from(updated_run),
            pr_url: pr.url,
            pr_number: pr.number,
            branch_name: pr.head_branch,
        }),
    ))
}

/// Continue the conversation with user feedback.
/// Uses the same Claude session (--continue) in the existing work directory.
/// Requires phase == "analyzed".
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/re-analyze",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 202, description = "Conversation continued with feedback"),
        (status = 400, description = "Run not in analyzed phase"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn re_analyze(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    // Verify the run is in the right phase
    let run = app_state
        .run_service
        .get_run(run_id)
        .await
        .map_err(Problem::from)?;

    if run.phase.as_deref() != Some("analyzed") {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Run {} cannot continue: expected phase 'analyzed', got '{}'",
                run_id,
                run.phase.as_deref().unwrap_or("none")
            ),
        }));
    }

    // Spawn conversation continuation in background (uses --continue, no re-clone)
    let autofixer = app_state.autofixer_service.clone();
    tokio::spawn(async move {
        autofixer.continue_with_feedback(run_id).await;
    });

    Ok((StatusCode::ACCEPTED, Json(AutofixerRunResponse::from(run))))
}

/// Cancel an autofixer run and clean up the work directory.
#[utoipa::path(
    tag = "Autofixer",
    post,
    path = "/projects/{project_id}/autofixer/runs/{run_id}/cancel",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("run_id" = i32, Path, description = "Run ID"),
    ),
    responses(
        (status = 200, description = "Run cancelled"),
        (status = 400, description = "Run is already in a terminal state"),
        (status = 404, description = "Run not found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, run_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);

    app_state
        .run_service
        .ensure_run_in_project(run_id, project_id)
        .await
        .map_err(Problem::from)?;

    app_state
        .autofixer_service
        .cancel_run(run_id)
        .await
        .map_err(Problem::from)?;

    let run = app_state
        .run_service
        .get_run(run_id)
        .await
        .map_err(Problem::from)?;

    app_state.telemetry.report(
        temps_core::TelemetryEvent::new(temps_core::TelemetryEventKind::AutofixerFixRejected)
            .with("source", run.source.clone())
            .with_opt("ai_provider", run.ai_provider.clone()),
    );

    Ok(Json(AutofixerRunResponse::from(run)))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use temps_entities::agent_runs;

    fn make_autofixer_run(
        id: i32,
        project_id: i32,
        status: &str,
        phase: Option<&str>,
    ) -> agent_runs::Model {
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

    #[test]
    fn test_autofixer_run_response_from_model() {
        let model = make_autofixer_run(1, 10, "analyzing", Some("analyzing"));
        let response = AutofixerRunResponse::from(model);

        assert_eq!(response.id, 1);
        assert_eq!(response.project_id, 10);
        assert_eq!(response.status, "analyzing");
        assert_eq!(response.phase, Some("analyzing".to_string()));
        assert!(response.analysis.is_none());
    }

    #[test]
    fn test_autofixer_run_response_with_analysis() {
        let mut model = make_autofixer_run(2, 10, "analyzed", Some("analyzed"));
        model.analysis = Some("Root cause: null pointer".to_string());
        let response = AutofixerRunResponse::from(model);

        assert_eq!(response.phase, Some("analyzed".to_string()));
        assert_eq!(
            response.analysis,
            Some("Root cause: null pointer".to_string())
        );
    }

    #[test]
    fn test_autofixer_run_response_fix_ready() {
        let mut model = make_autofixer_run(3, 10, "fix_ready", Some("fix_ready"));
        model.files_changed = 3;
        let response = AutofixerRunResponse::from(model);

        assert_eq!(response.status, "fix_ready");
        assert_eq!(response.phase, Some("fix_ready".to_string()));
        assert_eq!(response.files_changed, 3);
    }

    #[test]
    fn test_agent_error_to_problem_run_not_found() {
        let error = AgentError::RunNotFound { run_id: 99 };
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_agent_error_to_problem_validation() {
        let error = AgentError::Validation {
            message: "bad phase".to_string(),
        };
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_agent_error_to_problem_ai_cli_not_installed() {
        let error = AgentError::AiCliNotInstalled {
            provider: "claude_cli".to_string(),
        };
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn test_start_analysis_request_deserializes() {
        let json = r#"{"error_group_id": 7, "user_context": "user note"}"#;
        let req: StartAnalysisRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.error_group_id, 7);
        assert_eq!(req.user_context, Some("user note".to_string()));
    }

    #[test]
    fn test_start_analysis_request_without_context() {
        let json = r#"{"error_group_id": 5}"#;
        let req: StartAnalysisRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.error_group_id, 5);
        assert!(req.user_context.is_none());
    }

    #[test]
    fn test_add_context_request_deserializes() {
        let json = r#"{"message": "the bug appears only on prod"}"#;
        let req: AddContextRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "the bug appears only on prod");
    }
}
