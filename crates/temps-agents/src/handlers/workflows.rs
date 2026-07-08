//! Ephemeral workflow execution (CLI dry-run).
//!
//! Lets a developer upload a `WorkflowYamlConfig` from their machine and run
//! it once without committing it to `.temps/workflows/` or persisting a
//! `project_agents` row. The run is recorded in `agent_runs` with
//! `source = "cli_ephemeral"` so it shows up in the dashboard alongside
//! committed runs (with a badge), but it can't be retried server-side.
//!
//! Hard rules enforced here (defense in depth — the executor re-pins them):
//! - `deliverable` is forced to `"report"` (no PRs from arbitrary YAML).
//! - `max_turns`, `timeout_seconds`, `daily_budget_cents` are capped.
//! - YAML body size is capped before parsing to prevent memory abuse.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;

use crate::error::AgentError;
use crate::handlers::runs::AgentRunResponse;
use crate::handlers::AppState;

// ── Limits ────────────────────────────────────────────────────────────────────

/// Max YAML body size accepted by the dry-run endpoint. Larger payloads are
/// almost certainly abuse — real workflow YAMLs are well under this.
const MAX_EPHEMERAL_YAML_BYTES: usize = 64 * 1024;

/// Hard cap on `max_turns` for any ephemeral run, regardless of what the
/// uploaded YAML claims. Sits comfortably above the default (30) so a
/// dry-run can stretch its legs while still preventing runaway costs from
/// an unreviewed YAML. Note: committed workflows (via the agent CRUD path)
/// can opt into max_turns: 0 for unlimited; ephemeral dry-runs cannot
/// because nobody has signed off on the cost profile.
const MAX_EPHEMERAL_TURNS: i32 = 50;

/// Hard cap on `timeout_seconds` for any ephemeral run (15 minutes).
const MAX_EPHEMERAL_TIMEOUT_SECS: i32 = 900;

/// Hard cap on per-run cost. The dry-run endpoint is cheap to call; without a
/// ceiling, a buggy prompt could burn the project's daily budget on a single
/// invocation.
const MAX_EPHEMERAL_COST_CENTS: i32 = 200;

/// Hard cap on CPU cores for any ephemeral sandbox. A single dry-run should
/// never starve the host; clamp well below typical worker core counts.
const MAX_EPHEMERAL_CPU_CORES: f64 = 4.0;

/// Hard cap on sandbox memory (MB). Matches `AgentSandboxSettings`'s 8 GB
/// default — above this is almost certainly a misconfigured YAML.
const MAX_EPHEMERAL_MEMORY_MB: u64 = 8192;

// ── Request / Response ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct WorkflowDryRunRequest {
    /// Full WorkflowYamlConfig as YAML text. Server validates and re-serializes
    /// before storing on the run row.
    pub yaml: String,
    /// Optional context appended to the prompt (e.g. "test against staging
    /// only"). Mirrors `TriggerAgentRequest.user_context`.
    #[serde(default)]
    pub user_context: Option<String>,
    /// Optional CPU override applied after parsing YAML (clamped server-side).
    /// When `Some`, this takes precedence over `cpu_limit` inside the YAML —
    /// lets the CLI pass `--cpu` without rewriting the YAML text.
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    /// Optional memory override in MB (clamped server-side). Same precedence
    /// rule as `cpu_limit`.
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    /// Optional error group to link this dry-run to. When set, the executor's
    /// `load_error_context` path injects `{{error_type}}` / `{{error_message}}`
    /// / `{{stack_trace}}` into the prompt — same behaviour as a committed
    /// workflow triggered with `trigger_source_type = "error_group"`. Must
    /// belong to `project_id` (handler enforces).
    #[serde(default)]
    pub error_group_id: Option<i32>,
}

// ── Audit ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct WorkflowDryRunAudit {
    context: AuditContext,
    project_id: i32,
    run_id: i32,
    workflow_name: String,
    yaml_bytes: usize,
}

impl AuditOperation for WorkflowDryRunAudit {
    fn operation_type(&self) -> String {
        "WORKFLOW_DRY_RUN".to_string()
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

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/projects/{project_id}/workflows/dry-run",
        post(workflow_dry_run),
    )
}

// ── Handler ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Workflows",
    post,
    path = "/projects/{project_id}/workflows/dry-run",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = WorkflowDryRunRequest,
    responses(
        (status = 202, description = "Ephemeral run created and queued", body = AgentRunResponse),
        (status = 400, description = "Validation error (bad YAML, oversized payload, capped limits exceeded)"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn workflow_dry_run(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<WorkflowDryRunRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    // Verify the project exists. Skipping this would leak ephemeral runs into
    // the agent_runs table for nonexistent projects (FK would catch it, but
    // the error would be opaque).
    use sea_orm::EntityTrait;
    temps_entities::projects::Entity::find_by_id(project_id)
        .one(app_state.db.as_ref())
        .await
        .map_err(AgentError::Database)
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::from(AgentError::ProjectNotFound { project_id }))?;

    if request.yaml.len() > MAX_EPHEMERAL_YAML_BYTES {
        return Err(Problem::from(AgentError::Validation {
            message: format!(
                "Workflow YAML is {} bytes, exceeds limit of {}",
                request.yaml.len(),
                MAX_EPHEMERAL_YAML_BYTES
            ),
        }));
    }

    // Parse + validate the YAML server-side. The executor will re-parse this
    // when the run starts; parsing here lets us reject invalid YAML with a
    // 400 instead of having the run fail with an opaque "Validation" error
    // halfway through `execute_run_inner`.
    let mut config: temps_core::WorkflowYamlConfig =
        serde_yaml::from_str(&request.yaml).map_err(|e| {
            Problem::from(AgentError::Validation {
                message: format!("Invalid workflow YAML: {}", e),
            })
        })?;

    if config.name.trim().is_empty() {
        return Err(Problem::from(AgentError::Validation {
            message: "Workflow `name` is required and cannot be blank".to_string(),
        }));
    }
    if config.prompt.trim().is_empty() {
        return Err(Problem::from(AgentError::Validation {
            message: "Workflow `prompt` is required and cannot be blank".to_string(),
        }));
    }

    // Force-cap fields that affect cost/runtime. We mutate the parsed config
    // then re-serialize so the YAML stored on the run row reflects what
    // actually executed (the dashboard "View YAML" modal shows truth, not
    // what the user uploaded).
    if config.max_turns <= 0 || config.max_turns > MAX_EPHEMERAL_TURNS {
        config.max_turns = MAX_EPHEMERAL_TURNS.min(config.max_turns.max(1));
    }
    if config.timeout_seconds <= 0 || config.timeout_seconds > MAX_EPHEMERAL_TIMEOUT_SECS {
        config.timeout_seconds = MAX_EPHEMERAL_TIMEOUT_SECS.min(config.timeout_seconds.max(60));
    }
    if config.daily_budget_cents <= 0 || config.daily_budget_cents > MAX_EPHEMERAL_COST_CENTS {
        config.daily_budget_cents = MAX_EPHEMERAL_COST_CENTS;
    }

    // CPU / memory overrides: the top-level request fields win over the YAML
    // so the CLI can pass `--cpu` / `--memory` without rewriting the file.
    // Both paths are clamped against the ephemeral caps — values outside the
    // range are silently pinned rather than rejected, matching the existing
    // max_turns / timeout behaviour above.
    if let Some(cpu) = request.cpu_limit.or(config.cpu_limit) {
        let clamped = cpu.clamp(0.1, MAX_EPHEMERAL_CPU_CORES);
        config.cpu_limit = Some(clamped);
    }
    if let Some(mem) = request.memory_limit_mb.or(config.memory_limit_mb) {
        let clamped = mem.clamp(128, MAX_EPHEMERAL_MEMORY_MB);
        config.memory_limit_mb = Some(clamped);
    }

    // Force `report` deliverable: an ephemeral CLI run must never open a PR
    // or push a branch. The executor re-pins this in `synthesize_ephemeral_config`
    // so even a future bug here can't escalate the run's blast radius.
    config.deliverable = "report".to_string();

    let workflow_name = config.name.clone();
    let normalized_yaml = serde_yaml::to_string(&config).map_err(|e| {
        Problem::from(AgentError::Validation {
            message: format!("Failed to re-serialize normalized YAML: {}", e),
        })
    })?;

    // Validate error_group_id up front so the caller gets a 400 instead of
    // having the run fail mid-flight inside the executor. Cross-project
    // check: a user with ProjectsWrite on project A must not be able to
    // peek at project B's error groups via this endpoint.
    let trigger_source = if let Some(group_id) = request.error_group_id {
        use sea_orm::EntityTrait;
        let group = temps_entities::error_groups::Entity::find_by_id(group_id)
            .one(app_state.db.as_ref())
            .await
            .map_err(AgentError::Database)
            .map_err(Problem::from)?
            .ok_or_else(|| {
                Problem::from(AgentError::Validation {
                    message: format!("Error group {} not found", group_id),
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
        Some(("error_group".to_string(), group_id))
    } else {
        None
    };

    let run = app_state
        .run_service
        .create_ephemeral_run(
            project_id,
            normalized_yaml,
            request.user_context,
            trigger_source,
        )
        .await
        .map_err(Problem::from)?;

    // Spawn the executor — same fire-and-forget pattern as `trigger_agent`.
    let executor = app_state.executor.clone();
    let run_id = run.id;
    tokio::spawn(async move {
        executor.execute_run(run_id).await;
    });

    // Audit log: record who uploaded the YAML, with body size for abuse
    // analysis. The YAML content itself lives on the run row.
    let audit = WorkflowDryRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        run_id: run.id,
        workflow_name: workflow_name.clone(),
        yaml_bytes: request.yaml.len(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to audit workflow dry-run (project {}, run {}): {}",
            project_id,
            run.id,
            e
        );
    }

    let run_resp = AgentRunResponse::from_with_agent(
        run,
        Some(format!("ephemeral-{}", workflow_name)),
        Some(workflow_name),
    );

    Ok((StatusCode::ACCEPTED, Json(run_resp)))
}
