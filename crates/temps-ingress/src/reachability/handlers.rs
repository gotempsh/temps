// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `…/environments/{environment_id}/reachability` (ADR 064 D3, D4) and the
//! Cloudflare token check (D8).

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use temps_auth::permissions::Permission;
use temps_auth::{
    permission_guard, project_permission_guard, project_scope_guard, require_sensitive_action,
    AuthContext, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, AuditOperation, ProjectAccessChecker, RequestMetadata,
    SensitiveAction, SensitiveActionAuthorizer,
};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use temps_core::gate::GateKind;

use super::edge_rules::{EdgeRulePurpose, HttpPolicy};
use super::model::{
    ActionKind, AddressAudience, AddressReachability, AddressSource, ApplyRequest, Audience,
    AudienceChoice, DecideRequest, Decision, DnsRecordState, EnvironmentReachability, FindingKind,
    FindingLevel, Intent, JournalEntry, JournalState, PlanBlocked, PlanOption, PlanOptionKind,
    PlanRequest, PlanStep, PlannedRecord, PreviousChoice, ReachAction, ReachFinding, ReachState,
    ReachabilityChoices, ReachabilityPlan, ResumeRequest, RunResource, RunState, RunStep,
    RunStepStatus, RunView, StepKind, UndoResult, WayInChoice,
};
use super::plan::permissions_for;
use super::service::ReachabilityService;
use super::token_check::{
    PermissionCheck, PermissionState, TokenCheckRequest, TokenCheckResult, TokenPermission,
    ZoneScope,
};

pub struct ReachabilityAppState {
    pub service: Arc<ReachabilityService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

/// Whether HTTP-01 can issue a certificate here: a direct ingress answers
/// on port 80.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CertificateMethods {
    pub http01_available: bool,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_environment_reachability,
        get_reachability_choices,
        plan_environment_reachability,
        apply_environment_reachability,
        list_reachability_runs,
        get_reachability_run,
        resume_reachability_run,
        undo_reachability_run,
        decide_reachability_conflict,
        check_cloudflare_token,
        get_certificate_methods,
    ),
    components(schemas(
        EnvironmentReachability,
        AddressReachability,
        AddressSource,
        Audience,
        ReachState,
        ReachAction,
        ActionKind,
        ReachFinding,
        FindingKind,
        FindingLevel,
        EdgeRulePurpose,
        HttpPolicy,
        PlanRequest,
        ReachabilityPlan,
        ReachabilityChoices,
        AudienceChoice,
        WayInChoice,
        PlanOption,
        PlanOptionKind,
        PlanStep,
        StepKind,
        PlanBlocked,
        ApplyRequest,
        ResumeRequest,
        RunView,
        Intent,
        JournalEntry,
        JournalState,
        PreviousChoice,
        RunState,
        RunStep,
        RunStepStatus,
        RunResource,
        UndoResult,
        DecideRequest,
        Decision,
        DnsRecordState,
        PlannedRecord,
        AddressAudience,
        TokenCheckRequest,
        TokenCheckResult,
        PermissionCheck,
        PermissionState,
        TokenPermission,
        ZoneScope,
        CertificateMethods,
    )),
    tags((name = "Reachability", description = "Making a project reachable (ADR 064)"))
)]
pub struct ReachabilityApiDoc;

pub fn configure_routes() -> Router<Arc<ReachabilityAppState>> {
    let base = "/projects/{project_id}/environments/{environment_id}/reachability";
    Router::new()
        .route(base, get(get_environment_reachability))
        .route(&format!("{base}/choices"), get(get_reachability_choices))
        .route(&format!("{base}/plan"), post(plan_environment_reachability))
        .route(
            &format!("{base}/apply"),
            post(apply_environment_reachability),
        )
        .route(&format!("{base}/runs"), get(list_reachability_runs))
        .route(
            &format!("{base}/runs/{{run_id}}"),
            get(get_reachability_run),
        )
        .route(
            &format!("{base}/runs/{{run_id}}/resume"),
            post(resume_reachability_run),
        )
        .route(
            &format!("{base}/runs/{{run_id}}/undo"),
            post(undo_reachability_run),
        )
        .route(
            &format!("{base}/runs/{{run_id}}/decide"),
            post(decide_reachability_conflict),
        )
        .route(
            "/ingress/cloudflare/token-check",
            post(check_cloudflare_token),
        )
        .route("/ingress/certificate-methods", get(get_certificate_methods))
}

/// One audit record per run started or undone. Answers are never recorded:
/// one of them can be an API token.
#[derive(Debug, Serialize)]
struct ReachabilityAudit {
    #[serde(skip)]
    action: &'static str,
    context: AuditContext,
    project_id: i32,
    environment_id: i32,
    run_id: i32,
    option: String,
    address: Option<String>,
    details: serde_json::Value,
}

impl AuditOperation for ReachabilityAudit {
    fn operation_type(&self) -> String {
        self.action.to_string()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

async fn audit(
    state: &ReachabilityAppState,
    action: &'static str,
    auth: &AuthContext,
    metadata: &RequestMetadata,
    run: &RunView,
    details: serde_json::Value,
) {
    let record = ReachabilityAudit {
        action,
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id: run.project_id.unwrap_or_default(),
        environment_id: run.environment_id.unwrap_or_default(),
        run_id: run.id,
        option: run.option.clone(),
        address: run.address.clone(),
        details,
    };
    if let Err(error) = state.audit_service.create_audit_log(&record).await {
        error!(%error, action, "reachability change made but audit record failed");
    }
}

/// Refuse unless `auth` holds every permission in `needed`.
fn require_all(auth: &AuthContext, needed: &[Permission], what: &str) -> Result<(), Problem> {
    let missing: Vec<String> = needed
        .iter()
        .filter(|p| !auth.has_permission(p))
        .map(|p| p.to_string())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail(format!(
            "{what} needs the {} permission{}. Ask an administrator, or pick another way in.",
            missing.join(", "),
            if missing.len() == 1 { "" } else { "s" }
        )))
}

/// Whether visitors can reach each address of an environment.
///
/// Per address: who it is for, how traffic gets in, whether it works, why
/// not and what to do. Reachability is judged per address, not per node:
/// one ingress serves every app (ADR-020). When nothing is reachable the
/// response carries the plan *Make it reachable* would offer.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/reachability",
    operation_id = "get_environment_reachability",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, body = EnvironmentReachability),
        (status = 404, description = "Project or environment not found")
    ),
    security(("bearer_auth" = []))
)]
async fn get_environment_reachability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .environment(project_id, environment_id)
            .await?,
    ))
}

/// Which audiences, and which ways in, can work for an environment.
///
/// Per audience (and per gate for `signed_in`): whether any way in fits,
/// why not and what would fix it, the ways in a plan would offer with no
/// address asked for, and the address Temps would suggest (ADR 069 D3).
/// Runs the same planner as `…/reachability/plan` and stores nothing:
/// the answer is advisory, and `apply` still holds a run to the plan made
/// for the address the user chooses.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/choices",
    operation_id = "get_reachability_choices",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, body = ReachabilityChoices),
        (status = 404, description = "Project or environment not found")
    ),
    security(("bearer_auth" = []))
)]
async fn get_reachability_choices(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state.service.choices(project_id, environment_id).await?,
    ))
}

/// What *Make it reachable* would do for an address.
///
/// Returns the ways in that fit, in the order Temps prefers them, each with
/// its steps, the fields still to answer and, when an option changes
/// another choice, what to confirm. Changes nothing in the cluster; the plan
/// is kept for 15 minutes under `plan_id` with a fingerprint of what it
/// depends on, for `apply`.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/plan",
    operation_id = "plan_environment_reachability",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = PlanRequest,
    responses(
        (status = 200, body = ReachabilityPlan),
        (status = 404, description = "Project or environment not found")
    ),
    security(("bearer_auth" = []))
)]
async fn plan_environment_reachability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<PlanRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .plan(project_id, environment_id, request, Some(auth.user_id()))
            .await?,
    ))
}

/// Carry out one option of a stored plan.
///
/// Recomputes the plan's fingerprint first: when anything it depends on
/// changed (or the plan expired or was used), nothing is created and the
/// 409 carries the new plan under `plan`. Otherwise starts a run and returns
/// it at once; poll `…/reachability/runs/{run_id}` for progress. Every
/// resource is journaled before it is created, and a failed step removes
/// what the run made again, newest first.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/apply",
    operation_id = "apply_environment_reachability",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = ApplyRequest,
    responses(
        (status = 202, body = RunView),
        (status = 400, description = "An answer is missing or invalid"),
        (status = 403, description = "A permission the option needs is missing"),
        (status = 404, description = "Project or environment not found"),
        (status = 409, description = "The plan changed (the body's `plan` is the new one), or a run is in progress"),
        (status = 428, description = "Changing who an address is for, or publishing every environment under a tunnel, needs a recent MFA verification")
    ),
    security(("bearer_auth" = []))
)]
async fn apply_environment_reachability(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<ApplyRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    require_all(&auth, &permissions_for(request.option), "This way in")?;
    // Changing who an existing address is for is a sensitive action, both
    // ways (ADR 064 Security), and so is its gate or sign-in list (ADR 065
    // D7).
    // Publishing every environment under a tunnel widens all of them to
    // public at once (ADR 068 D3).
    if request.option == PlanOptionKind::TunnelEnvironmentHosts {
        require_sensitive_action(
            state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::PublishEnvironmentHosts { connector_id: None },
        )
        .await?;
    }
    let change = state
        .service
        .audience_change(
            project_id,
            environment_id,
            &request.plan_id,
            request.option,
            &request.answers,
        )
        .await?;
    // ADR 065 D9: a Cloudflare Access plan creates or uses an Access app
    // with a saved token.
    if change
        .as_ref()
        .is_some_and(|(_, _, after)| after.gate == Some(GateKind::CloudflareAccess))
    {
        require_all(
            &auth,
            &[Permission::DnsProvidersWrite],
            "Putting Cloudflare Access in front of it",
        )?;
    }
    if let Some(action) = audience_action(change) {
        require_sensitive_action(state.sensitive_action_authorizer.as_ref(), &auth, action).await?;
    }
    let option = request.option;
    let email = auth.user.as_ref().map(|u| u.email.clone());
    let run = state
        .service
        .apply(
            project_id,
            environment_id,
            request,
            Some(auth.user_id()),
            email,
        )
        .await?;
    audit(
        &state,
        "REACHABILITY_RUN_STARTED",
        &auth,
        &metadata,
        &run,
        serde_json::json!({ "option": option.as_str() }),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// The newest runs of an environment.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/runs",
    operation_id = "list_reachability_runs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses((status = 200, body = Vec<RunView>)),
    security(("bearer_auth" = []))
)]
async fn list_reachability_runs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(state.service.runs(project_id, environment_id).await?))
}

/// One run, with the progress of each step and what it created.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/runs/{run_id}",
    operation_id = "get_reachability_run",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = i32, Path, description = "Run ID")
    ),
    responses(
        (status = 200, body = RunView),
        (status = 404, description = "Run not found")
    ),
    security(("bearer_auth" = []))
)]
async fn get_reachability_run(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .run(project_id, environment_id, run_id)
            .await?,
    ))
}

/// Carry on a waiting or interrupted run from its first step not done.
///
/// What earlier attempts made is looked up and reused, never made twice.
/// Answer `cloudflare_api_token` again when the run had not saved the
/// account yet: the token is never stored before that.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/runs/{run_id}/resume",
    operation_id = "resume_reachability_run",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = i32, Path, description = "Run ID")
    ),
    request_body = ResumeRequest,
    responses(
        (status = 202, body = RunView),
        (status = 400, description = "The token is needed again, or is not usable"),
        (status = 403, description = "A permission the run needs is missing"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run is going, recovering, or finished")
    ),
    security(("bearer_auth" = []))
)]
async fn resume_reachability_run(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, i32)>,
    Json(request): Json<ResumeRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let current = state
        .service
        .run(project_id, environment_id, run_id)
        .await?;
    if let Some(kind) = PlanOptionKind::parse(&current.option) {
        require_all(&auth, &permissions_for(kind), "Resuming this run")?;
    }
    let run = state
        .service
        .resume(project_id, environment_id, run_id, request)
        .await?;
    audit(
        &state,
        "REACHABILITY_RUN_RESUMED",
        &auth,
        &metadata,
        &run,
        serde_json::json!({ "option": run.option }),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Remove exactly what a run created, newest first.
///
/// What the run found already there stays, and a hostname choice it changed
/// is put back as it was. Refused while Temps is still recovering an
/// interrupted run. Removing a certificate needs a recent password or
/// passkey confirmation.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/runs/{run_id}/undo",
    operation_id = "undo_reachability_run",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = i32, Path, description = "Run ID")
    ),
    responses(
        (status = 200, body = RunView),
        (status = 403, description = "A permission or step-up confirmation is missing"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run is still going, or nothing is left to undo")
    ),
    security(("bearer_auth" = []))
)]
async fn undo_reachability_run(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let pending = state
        .service
        .undo_pending(project_id, environment_id, run_id)
        .await?;
    require_all(&auth, &undo_permissions(&pending), "Undoing this run")?;
    if let Some(domain) = pending.iter().find_map(|r| match r {
        RunResource::Certificate { domain } => Some(domain.clone()),
        _ => None,
    }) {
        require_sensitive_action(
            state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::DeleteDomain { domain },
        )
        .await?;
    }
    if let Some(action) = undo_audience_action(&pending) {
        require_sensitive_action(state.sensitive_action_authorizer.as_ref(), &auth, action).await?;
    }
    let run = state
        .service
        .undo(project_id, environment_id, run_id)
        .await?;
    audit(
        &state,
        "REACHABILITY_RUN_UNDONE",
        &auth,
        &metadata,
        &run,
        serde_json::json!({
            "removed": pending.iter().map(RunResource::describe).collect::<Vec<_>>(),
            "state": run.state.as_str(),
        }),
    )
    .await;
    Ok(Json(run))
}

/// Answer a conflict: a DNS record someone else changed while the run was
/// setting it, or after it did (ADR 064 D4).
///
/// Going forward, `keep` leaves their value and skips the step, and
/// `overwrite` lets Temps set it; the run is then `interrupted`, to resume.
/// While undoing, `keep` leaves their value and `overwrite` restores what
/// was there before the run; undo then carries on, with undo's checks.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/reachability/runs/{run_id}/decide",
    operation_id = "decide_reachability_conflict",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("run_id" = i32, Path, description = "Run ID")
    ),
    request_body = DecideRequest,
    responses(
        (status = 200, body = RunView),
        (status = 400, description = "That entry is not waiting for a decision"),
        (status = 403, description = "A permission or step-up confirmation is missing"),
        (status = 404, description = "Run not found"),
        (status = 409, description = "The run is not waiting for a decision")
    ),
    security(("bearer_auth" = []))
)]
async fn decide_reachability_conflict(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, run_id)): Path<(i32, i32, i32)>,
    Json(request): Json<DecideRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let current = state
        .service
        .run(project_id, environment_id, run_id)
        .await?;
    // Writing the record either way needs what the run's DNS step needed;
    // carrying on with undo needs what undo needs.
    require_all(
        &auth,
        &[Permission::DnsProvidersWrite],
        "Deciding this conflict",
    )?;
    if current.state == RunState::UndoPaused {
        let pending = state
            .service
            .undo_pending(project_id, environment_id, run_id)
            .await?;
        require_all(&auth, &undo_permissions(&pending), "Undoing this run")?;
        if let Some(domain) = pending.iter().find_map(|r| match r {
            RunResource::Certificate { domain } => Some(domain.clone()),
            _ => None,
        }) {
            require_sensitive_action(
                state.sensitive_action_authorizer.as_ref(),
                &auth,
                SensitiveAction::DeleteDomain { domain },
            )
            .await?;
        }
        if let Some(action) = undo_audience_action(&pending) {
            require_sensitive_action(state.sensitive_action_authorizer.as_ref(), &auth, action)
                .await?;
        }
    }
    // ADR 065 D4: deleting a Tailscale Service is confirmed with step-up,
    // and journaled with the definition the user was shown.
    let shown = state
        .service
        .service_to_delete(project_id, environment_id, run_id, &request)
        .await?;
    if let Some((tailnet, name, _)) = &shown {
        require_sensitive_action(
            state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::DeleteTailscaleService {
                tailnet: tailnet.clone(),
                name: name.clone(),
            },
        )
        .await?;
    }
    let key = request.key.clone();
    let decision = request.decision;
    let run = state
        .service
        .decide(project_id, environment_id, run_id, request)
        .await?;
    if let Some((tailnet, name, definition)) = shown {
        audit(
            &state,
            "TAILSCALE_SERVICE_DELETE_CONFIRMED",
            &auth,
            &metadata,
            &run,
            serde_json::json!({ "tailnet": tailnet, "service": name, "shown": definition }),
        )
        .await;
    }
    audit(
        &state,
        "REACHABILITY_CONFLICT_DECIDED",
        &auth,
        &metadata,
        &run,
        serde_json::json!({ "key": key, "decision": decision, "state": run.state.as_str() }),
    )
    .await;
    Ok(Json(run))
}

/// The step-up an apply needs when its option changes an existing
/// address's audience.
fn audience_action(
    change: Option<(String, AddressAudience, AddressAudience)>,
) -> Option<SensitiveAction> {
    let (hostname, before, after) = change?;
    if before.who() == after.who() {
        return None;
    }
    // ADR 065 D6: editing who a `tailnet_users` address admits.
    if before.audience == Audience::TailnetUsers && after.audience == Audience::TailnetUsers {
        return Some(SensitiveAction::ChangeTailnetPrincipals { hostname });
    }
    Some(SensitiveAction::ChangeAddressAudience {
        hostname,
        from: before.who_label(),
        to: after.who_label(),
    })
}

/// The step-up an undo needs when it puts an address back to another
/// audience. An address the run attached itself is removed instead, and
/// stays private until then, so that needs none.
fn undo_audience_action(pending: &[RunResource]) -> Option<SensitiveAction> {
    pending.iter().find_map(|resource| match resource {
        RunResource::AddressAudience {
            hostname,
            before,
            after,
        } if before.who() != after.who()
            && !(before.audience == Audience::TailnetUsers
                && after.audience == Audience::TailnetUsers)
            && !pending.iter().any(
                |r| matches!(r, RunResource::CustomDomain { domain, .. } if domain == hostname),
            ) =>
        {
            Some(SensitiveAction::ChangeAddressAudience {
                hostname: hostname.clone(),
                from: after.who_label(),
                to: before.who_label(),
            })
        }
        // ADR 065 D6: undo puts an earlier list back.
        RunResource::AddressAudience {
            hostname,
            before,
            after,
        } if before.audience == Audience::TailnetUsers
            && after.audience == Audience::TailnetUsers
            && before.who() != after.who() =>
        {
            Some(SensitiveAction::ChangeTailnetPrincipals {
                hostname: hostname.clone(),
            })
        }
        _ => None,
    })
}

/// What removing `pending` needs, on top of `projects:write`.
fn undo_permissions(pending: &[RunResource]) -> Vec<Permission> {
    let mut needed = Vec::new();
    for resource in pending {
        let permission = match resource {
            RunResource::DnsProvider { .. }
            | RunResource::ManagedDomain { .. }
            | RunResource::AutoManageEnabled { .. }
            | RunResource::DnsRecord { .. }
            | RunResource::DnsChange { .. } => Permission::DnsProvidersWrite,
            RunResource::Connector { .. }
            | RunResource::HostnameChoice { .. }
            | RunResource::EnvironmentHosts { .. } => Permission::SettingsWrite,
            // Part of publishing through a tunnel (ADR 070), which applying
            // `existing_tunnel` needs settings:write for; undo needs the same.
            RunResource::EdgeRule { .. } => Permission::SettingsWrite,
            RunResource::CustomDomain { .. }
            | RunResource::AddressAudience { .. }
            | RunResource::HttpPolicy { .. }
            | RunResource::TailscaleServe { .. }
            | RunResource::TailscaleService { .. } => Permission::ProjectsWrite,
            RunResource::TailscaleCredential { .. } => Permission::SettingsWrite,
            RunResource::Certificate { .. } => Permission::DomainsDelete,
            RunResource::AccessApp { .. } => Permission::DnsProvidersWrite,
        };
        if !needed.contains(&permission) {
            needed.push(permission);
        }
    }
    needed
}

/// Check which permissions a pasted Cloudflare API token has.
///
/// Read-only calls, plus two writes whose bodies Cloudflare is certain to
/// reject (an A record with an invalid address, a tunnel with no name):
/// "not authorized" means the permission is missing, a validation error
/// that it is there, and nothing is created either way. Any other answer is
/// `unverified`, and a run refuses the token. A token that reaches zones
/// besides the one asked about is refused too, naming them. The token is
/// used for this check only and is not stored.
#[utoipa::path(
    tag = "Reachability",
    post,
    path = "/ingress/cloudflare/token-check",
    operation_id = "check_cloudflare_token",
    request_body = TokenCheckRequest,
    responses((status = 200, body = TokenCheckResult)),
    security(("bearer_auth" = []))
)]
async fn check_cloudflare_token(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
    Json(request): Json<TokenCheckRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DnsProvidersWrite);
    Ok(Json(state.service.token_check(request).await))
}

/// Which certificate methods fit this cluster.
///
/// HTTP-01 needs an ingress that answers on port 80; without one, issue
/// certificates through DNS-01.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/ingress/certificate-methods",
    operation_id = "get_certificate_methods",
    responses((status = 200, body = CertificateMethods)),
    security(("bearer_auth" = []))
)]
async fn get_certificate_methods(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ReachabilityAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, DomainsRead);
    Ok(Json(CertificateMethods {
        http01_available: state.service.http01_available().await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::gate::{GateAllowList, GateKind};

    fn change(
        before: AddressAudience,
        after: AddressAudience,
    ) -> Option<(String, AddressAudience, AddressAudience)> {
        Some(("app.example.com".into(), before, after))
    }

    fn list(teams: Vec<i32>, users: Vec<i32>) -> Option<GateAllowList> {
        Some(GateAllowList { teams, users })
    }

    /// ADR 065 D7: changing the audience, the gate or the list of who may
    /// sign in is a sensitive action; keeping all three the same is not.
    #[test]
    fn changing_who_can_open_an_address_needs_a_step_up() {
        let signed_in = AddressAudience::signed_in(GateKind::Temps, None);
        let listed = AddressAudience::signed_in(GateKind::Temps, list(vec![2], vec![]));
        for (before, after) in [
            (AddressAudience::public(), signed_in.clone()),
            (signed_in.clone(), AddressAudience::public()),
            (signed_in.clone(), listed.clone()),
            (listed.clone(), signed_in.clone()),
            (
                listed.clone(),
                AddressAudience::signed_in(GateKind::Temps, list(vec![2], vec![9])),
            ),
            (
                signed_in.clone(),
                AddressAudience::signed_in(GateKind::CloudflareAccess, None),
            ),
        ] {
            let label = format!("{} -> {}", before.who_label(), after.who_label());
            assert!(
                matches!(
                    audience_action(change(before, after)),
                    Some(SensitiveAction::ChangeAddressAudience { .. })
                ),
                "no step-up for {label}"
            );
        }
    }

    #[test]
    fn the_same_people_in_another_order_is_not_a_change() {
        let before = AddressAudience::signed_in(GateKind::Temps, list(vec![3, 1], vec![7]));
        let after = AddressAudience::signed_in(GateKind::Temps, list(vec![1, 3, 3], vec![7]));
        assert!(audience_action(change(before, after)).is_none());
        assert!(
            audience_action(change(AddressAudience::public(), AddressAudience::public())).is_none()
        );
        assert!(audience_action(None).is_none());
    }

    #[test]
    fn undoing_a_gate_change_needs_a_step_up_unless_the_run_attached_the_address() {
        let gate = RunResource::AddressAudience {
            hostname: "app.example.com".into(),
            before: AddressAudience::public(),
            after: AddressAudience::signed_in(GateKind::Temps, list(vec![1], vec![])),
        };
        assert!(undo_audience_action(std::slice::from_ref(&gate)).is_some());
        let attached = RunResource::CustomDomain {
            id: 4,
            domain: "app.example.com".into(),
        };
        assert!(undo_audience_action(&[attached, gate]).is_none());
    }
    use temps_core::tailnet::TailnetPrincipal;

    fn users(values: &[&str]) -> AddressAudience {
        AddressAudience {
            audience: Audience::TailnetUsers,
            private_via: Some("tailscale_serve".into()),
            principals: values
                .iter()
                .map(|v| TailnetPrincipal::parse(v).unwrap())
                .collect(),
            ..AddressAudience::default()
        }
    }

    #[test]
    fn editing_who_a_tailnet_users_address_admits_needs_step_up() {
        let host = "homelab.example.ts.net".to_string();
        let before = users(&["alice@example.com"]);
        let wider = users(&["alice@example.com", "bob@example.com"]);
        assert_eq!(
            audience_action(Some((host.clone(), before.clone(), wider.clone()))),
            Some(SensitiveAction::ChangeTailnetPrincipals {
                hostname: host.clone()
            })
        );
        // The same list again changes nothing.
        assert_eq!(
            audience_action(Some((host.clone(), before.clone(), before.clone()))),
            None
        );
        // The same people in another order is no change either.
        assert_eq!(
            audience_action(Some((
                host.clone(),
                users(&["bob@example.com", "alice@example.com"]),
                wider.clone()
            ))),
            None
        );
        // Another audience is the audience change it always was.
        assert!(matches!(
            audience_action(Some((
                host,
                before,
                AddressAudience {
                    audience: Audience::Private,
                    ..AddressAudience::default()
                }
            ))),
            Some(SensitiveAction::ChangeAddressAudience { ref from, ref to, .. })
                if from == "tailnet_users" && to == "private"
        ));
    }

    #[test]
    fn undo_putting_an_earlier_list_back_needs_step_up() {
        let pending = vec![RunResource::AddressAudience {
            hostname: "homelab.example.ts.net".into(),
            before: users(&["alice@example.com", "bob@example.com"]),
            after: users(&["alice@example.com"]),
        }];
        assert_eq!(
            undo_audience_action(&pending),
            Some(SensitiveAction::ChangeTailnetPrincipals {
                hostname: "homelab.example.ts.net".into()
            })
        );
        let unchanged = vec![RunResource::AddressAudience {
            hostname: "homelab.example.ts.net".into(),
            before: users(&["alice@example.com"]),
            after: users(&["alice@example.com"]),
        }];
        assert_eq!(undo_audience_action(&unchanged), None);
    }
}
