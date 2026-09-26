// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The operator path onto the bulk Cloud-telemetry activation engine
//! (ADR-042 §9).
//!
//! Five endpoints over the engine ADR-042 P1 built:
//!
//! ```text
//! POST   /otel/cloud-telemetry/bulk-jobs/estimate      quote it, send nothing
//! POST   /otel/cloud-telemetry/bulk-jobs               confirm the quote
//! GET    /otel/cloud-telemetry/bulk-jobs/{batch_id}    progress and ETA
//! GET    /otel/cloud-telemetry/bulk-jobs/current       the running job, or null
//! POST   /otel/cloud-telemetry/bulk-jobs/{batch_id}/cancel
//! ```
//!
//! # Why estimate and submit are two calls
//!
//! Egress costs the customer real money, and the operator path — unlike the
//! purchase path — has no payment event behind it (ADR-042 §1). So the quote is
//! computed first, sends nothing, and hands back a signed
//! [`plan_token`](crate::services::cloud_bulk_activation_plan) over exactly the
//! projects, windows and estimates it displayed. `POST /bulk-jobs` takes that
//! token and nothing else, so the job that gets created is provably the one that
//! was quoted rather than approximately it.
//!
//! # Why every read answers, in every state
//!
//! `GET /bulk-jobs/current` returns `200` with a `null` body when no job is
//! running, and `/estimate` answers on an unlinked instance with
//! `configured: false` plus a reason and a setup path. A `404` cannot be told
//! apart from "this feature does not exist", and those need completely different
//! UI. The Console's activation card renders in every one of these states, so
//! none of them may be an error.
//!
//! # ETA
//!
//! `remaining_spans / observed_throughput`, where throughput is the job's own
//! average since `started_at`. Before the first chunk acknowledges there is no
//! measured rate, so [`BulkActivationEtaState::Estimating`] is returned with a
//! `null` `eta_seconds` — the client renders "estimating…" rather than a number
//! this instance made up.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::handlers::cloud_telemetry_handler::{
    instance_capability, link_snapshot, local_retention_days,
};
use crate::services::cloud_backfill::{estimate_backfill, CloudBackfillError};
use crate::services::cloud_bulk_activation::{
    BulkJobDetail, BulkJobProjectPlan, BulkSkipReason, CloudBulkActivationError,
    EnqueueBulkJobRequest,
};
use crate::services::cloud_bulk_activation_plan::{
    mint_plan_token, verify_plan_token, PlanTokenError,
};
use crate::services::cloud_fidelity::CloudPolicyError;
use crate::services::telemetry_write_mode::CLOUD_SETUP_PATH;
use crate::OtelAppState;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, DBDateTime, ProblemDetails, RequestMetadata};
use temps_entities::cloud_telemetry_bulk_job_projects::{
    BulkJobProjectStatus, Model as BulkJobProject,
};
use temps_entities::cloud_telemetry_bulk_jobs::{BulkJobStatus, BulkJobTrigger, Model as BulkJob};
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;

/// Ceiling on how many projects one estimate may name.
///
/// Matches the plan token's own bound, so a request that would mint an
/// unsignable plan is refused before any counting query runs rather than after.
const MAX_ESTIMATE_PROJECTS: usize = crate::services::cloud_bulk_activation_plan::MAX_PLAN_PROJECTS;

// ── Error mapping ──────────────────────────────────────────────────────────

impl From<CloudBulkActivationError> for Problem {
    fn from(error: CloudBulkActivationError) -> Self {
        match error {
            // 409, and it carries the in-flight job's id as a value rather than
            // only inside the sentence. The caller's next move is to watch or
            // cancel *that* job, and it cannot do either from prose.
            CloudBulkActivationError::JobAlreadyActive { job_id, status } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .type_("https://temps.sh/probs/bulk-activation-job-already-active")
                    .title("A Cloud Telemetry Activation Is Already Running")
                    .detail(
                        CloudBulkActivationError::JobAlreadyActive { job_id, status }.to_string(),
                    )
                    .value("batch_id", job_id.to_string())
                    .value("status", status.to_string())
                    .value(
                        "status_path",
                        format!("/otel/cloud-telemetry/bulk-jobs/{job_id}"),
                    )
                    .build()
            }

            CloudBulkActivationError::JobNotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Activation Job Not Found")
                    .with_detail(error.to_string())
            }

            CloudBulkActivationError::JobProjectNotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Activation Job Project Not Found")
                    .with_detail(error.to_string())
            }

            CloudBulkActivationError::NoProjects
            | CloudBulkActivationError::DuplicateProject { .. }
            | CloudBulkActivationError::InvalidWindow { .. } => {
                problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
                    .with_title("Invalid Activation Plan")
                    .with_detail(error.to_string())
            }

            CloudBulkActivationError::Job { .. } | CloudBulkActivationError::Store { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Activation Job Store Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

impl From<PlanTokenError> for Problem {
    fn from(error: PlanTokenError) -> Self {
        match error {
            // Every one of these is "re-estimate", and each says so in its own
            // message. 400 rather than 403: nothing here is a permission
            // problem, and rendering it as one sends the operator to look at
            // their role instead of clicking the button that fixes it.
            PlanTokenError::Malformed { .. }
            | PlanTokenError::SignatureMismatch
            | PlanTokenError::Expired { .. }
            | PlanTokenError::UnsupportedVersion { .. }
            | PlanTokenError::NoProjects
            | PlanTokenError::TooManyProjects { .. } => {
                ErrorBuilder::new(axum::http::StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/bulk-activation-plan-token")
                    .title("Activation Plan Must Be Re-Estimated")
                    .detail(error.to_string())
                    .value(
                        "re_estimate_path",
                        "/otel/cloud-telemetry/bulk-jobs/estimate",
                    )
                    .build()
            }

            PlanTokenError::SigningFailed => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Could Not Issue An Activation Plan")
                    .with_detail(error.to_string())
            }
        }
    }
}

impl From<CloudBackfillError> for Problem {
    fn from(error: CloudBackfillError) -> Self {
        match error {
            // The three link-level refusals are the instance not being ready,
            // not the request being wrong — same 409 + `setup_path` shape the
            // per-project write-mode gate already uses.
            CloudBackfillError::NotLinked { .. }
            | CloudBackfillError::TelemetryExportDisabled { .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Temps Cloud Is Not Ready For Telemetry")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("setup_path", CLOUD_SETUP_PATH)
                    .build()
            }
            CloudBackfillError::FidelityNotQueryable { project_id, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Telemetry Fidelity Too Low To Estimate")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("blocked_by", "fidelity")
                    .value("setup_path", format!("/projects/{project_id}/settings"))
                    .build()
            }
            CloudBackfillError::SubmissionScopeBusy { .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Another Temps Cloud Submission Is In Flight")
                    .detail(error.to_string())
                    .build()
            }

            CloudBackfillError::Database { .. }
            | CloudBackfillError::ClickHouse { .. }
            | CloudBackfillError::ProjectionFailed { .. }
            | CloudBackfillError::ShipmentRefused { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Could Not Estimate The Activation")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ── DTOs ───────────────────────────────────────────────────────────────────

/// What to quote.
///
/// Exactly one of `all_eligible_projects` and a non-empty `project_ids` must be
/// given. Defaulting an omitted scope to "everything" would make a typo cost
/// money; defaulting it to "nothing" would make the endpoint silently useless.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct EstimateBulkActivationRequest {
    /// Quote every project that still writes its spans to this instance.
    #[serde(default)]
    pub all_eligible_projects: bool,
    /// Quote exactly these projects. Projects that are already Cloud-primary
    /// are accepted here — re-shipping a window is the retry path — but are
    /// never picked up by `all_eligible_projects`.
    #[serde(default)]
    pub project_ids: Option<Vec<i32>>,
    /// Start of the window to ship. Defaults to the oldest span local retention
    /// can still be holding.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub window_from: Option<DBDateTime>,
    /// End of the window to ship. Defaults to now.
    #[serde(default)]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub window_to: Option<DBDateTime>,
}

/// One project's line on the quote.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActivationProjectEstimateResponse {
    pub project_id: i32,
    /// Whether this project is part of the plan the token covers.
    pub eligible: bool,
    /// Machine-readable reason it is not, e.g. `fidelity_not_queryable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// The same reason as a sentence, so no client has to map an enum to prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_detail: Option<String>,
    /// Where the operator goes to unblock it, when anywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
    pub fidelity: CloudTelemetryFidelity,
    #[schema(value_type = String, format = DateTime)]
    pub window_from: DBDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub window_to: DBDateTime,
    /// Exact count of local spans in the window. Zero for a skipped project —
    /// nothing was counted, because nothing would be sent.
    pub estimated_spans: u64,
    /// `average_span_bytes * estimated_spans`, rounded up. Temps Cloud's own
    /// acknowledgement is authoritative; this is what this instance can know
    /// before sending.
    pub estimated_bytes: u64,
    /// How many spans were actually projected to derive the average.
    pub sampled_spans: u64,
    pub average_span_bytes: f64,
}

/// The quote.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActivationEstimateResponse {
    /// Whether a bulk activation could run at all right now.
    ///
    /// `false` is the normal state on an unlinked instance and must render as
    /// onboarding, not as an error.
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,

    #[schema(value_type = String, format = DateTime)]
    pub window_from: DBDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub window_to: DBDateTime,

    /// Every project considered, eligible or not, ascending by id.
    pub projects: Vec<BulkActivationProjectEstimateResponse>,
    pub total_projects: usize,
    pub eligible_projects: usize,
    pub skipped_projects: usize,

    /// Totals over the **eligible** projects only — what confirming this quote
    /// would actually send.
    pub estimated_spans: u64,
    pub estimated_bytes: u64,

    /// The handle to send back to `POST /bulk-jobs`.
    ///
    /// `None` when nothing is eligible: there is no bill to confirm, and
    /// handing back a token that would be refused on submit would be a dead
    /// end with no explanation attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_token: Option<String>,
    /// Stable identity of the project set and windows, recorded on the job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub plan_expires_at: Option<DBDateTime>,

    /// A job already running. Submitting would be refused with `409`, so the
    /// client can offer "watch the running job" instead of a button that fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub active_batch_id: Option<Uuid>,
}

/// Body for `POST /bulk-jobs`.
///
/// Deliberately one field. The plan is inside the token, so there is no project
/// list here to disagree with the one that was quoted.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateBulkActivationJobRequest {
    /// The `plan_token` from a `POST /bulk-jobs/estimate` response.
    pub plan_token: String,
}

/// How much of the ETA this instance can honestly claim to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkActivationEtaState {
    /// No chunk has acknowledged yet, so there is no observed throughput.
    /// `eta_seconds` is `null` and the client must say "estimating…" rather
    /// than showing a number.
    Estimating,
    /// `eta_seconds` is derived from this job's own observed throughput.
    Known,
    /// The job has stopped; there is nothing left to estimate.
    Finished,
}

/// One project's row inside a job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActivationJobProjectResponse {
    pub project_id: i32,
    pub status: BulkJobProjectStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub window_from: DBDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub window_to: DBDateTime,
    pub estimated_spans: i64,
    pub estimated_bytes: i64,
    pub spans_shipped: i64,
    pub bytes_shipped: i64,
    /// `spans_shipped / estimated_spans`, clamped to 0–100. `None` when the
    /// estimate is zero or unknown — an empty window is not "0% done".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_complete: Option<f64>,
    /// Why this project stopped, when `status` is `failed`. The switch is never
    /// rolled back, so this project is Cloud-primary with a recorded hole in
    /// its history — which is retryable, and must be visible to be retried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DBDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub completed_at: Option<DBDateTime>,
}

/// A job, its projects, its progress and its ETA.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkActivationJobResponse {
    #[schema(value_type = String)]
    pub batch_id: Uuid,
    pub trigger: BulkJobTrigger,
    pub status: BulkJobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by_user_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,

    pub estimated_spans: i64,
    pub estimated_bytes: i64,
    pub spans_shipped: i64,
    pub bytes_shipped: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_complete: Option<f64>,

    /// Seconds remaining, or `null` when `eta_state` is not `known`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<i64>,
    pub eta_state: BulkActivationEtaState,
    /// The average this job has actually achieved since it started. Sent so the
    /// client can render a coarse rate instead of implying a precision the
    /// average does not have.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_spans_per_sec: Option<f64>,

    /// The project being switched or backfilled right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_project_id: Option<i32>,

    pub projects_total: usize,
    pub projects_pending: usize,
    pub projects_done: usize,
    pub projects_failed: usize,
    pub projects_skipped: usize,

    /// Set as soon as a cancel is requested, before the worker honours it at
    /// the next chunk boundary — so the UI can stop offering Cancel twice.
    pub cancel_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub cancel_requested_at: Option<DBDateTime>,

    /// Machine-readable instance-wide abort reason, e.g. `not_linked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<String>,
    /// The same reason as a sentence naming the fix and the page that applies
    /// it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_detail: Option<String>,

    #[schema(value_type = String, format = DateTime)]
    pub created_at: DBDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DBDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub completed_at: Option<DBDateTime>,

    pub projects: Vec<BulkActivationJobProjectResponse>,
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// Quote a bulk Cloud-telemetry activation. Sends nothing.
#[utoipa::path(
    tag = "OTel",
    post,
    path = "/otel/cloud-telemetry/bulk-jobs/estimate",
    request_body = EstimateBulkActivationRequest,
    responses(
        (status = 200, description = "Per-project and total estimate, plus a plan token", body = BulkActivationEstimateResponse),
        (status = 400, description = "Neither or both scopes given, or an invalid window", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "Temps Cloud is not ready for telemetry", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn estimate_bulk_activation(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Json(request): Json<EstimateBulkActivationRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Deliberately `OtelWrite`: this is the read half of an action that stops
    // whole projects storing their spans on this machine.
    permission_guard!(auth, OtelWrite);
    instance_admin_guard(&auth)?;

    let now = chrono::Utc::now();
    // Both of these are properties of the request, not of the instance, so they
    // are settled before the Cloud link is consulted. A malformed request told
    // "Temps Cloud is not connected" would send the operator to fix the one
    // thing that is not wrong.
    let (window_from, window_to) = resolve_estimate_window(&request, now)?;
    let scope = validate_scope(&request)?;

    let link = link_snapshot(&state);
    let (configured, reason, setup_path) = instance_capability(&link);

    // An unlinked instance still answers, with the scope it was asked about and
    // an empty quote. The activation card renders from this — a 409 here would
    // make the card show an error where an onboarding state belongs.
    let Some(cloud_link) = state.cloud_link.clone().filter(|_| configured) else {
        return Ok(Json(BulkActivationEstimateResponse {
            configured: false,
            reason: reason.or_else(|| Some(unlinked_reason())),
            setup_path: setup_path.or_else(|| Some(CLOUD_SETUP_PATH.to_string())),
            window_from,
            window_to,
            projects: Vec::new(),
            total_projects: 0,
            eligible_projects: 0,
            skipped_projects: 0,
            estimated_spans: 0,
            estimated_bytes: 0,
            plan_token: None,
            plan_hash: None,
            plan_expires_at: None,
            active_batch_id: None,
        }));
    };

    // Compiled without a span source there is nothing to read history out of.
    // Same shape as "not linked": a state the card can render, not an error.
    let Some(source) = state.cloud_backfill_source.clone() else {
        return Ok(Json(BulkActivationEstimateResponse {
            configured: false,
            reason: Some(
                "This build has no local span source wired, so there is no history to ship to \
                 Temps Cloud. Projects can still be switched one at a time from their own \
                 settings."
                    .to_string(),
            ),
            setup_path: Some(CLOUD_SETUP_PATH.to_string()),
            window_from,
            window_to,
            projects: Vec::new(),
            total_projects: 0,
            eligible_projects: 0,
            skipped_projects: 0,
            estimated_spans: 0,
            estimated_bytes: 0,
            plan_token: None,
            plan_hash: None,
            plan_expires_at: None,
            active_batch_id: None,
        }));
    };

    let candidates = resolve_candidates(&state, scope).await?;

    let mut rows: Vec<BulkActivationProjectEstimateResponse> = Vec::with_capacity(candidates.len());
    let mut plan: Vec<BulkJobProjectPlan> = Vec::with_capacity(candidates.len());

    for project_id in candidates {
        let policy = match state.otel_service.cloud_policy_cache() {
            Some(cache) => match cache.resolve_project(project_id).await {
                Ok(policy) => policy,
                Err(CloudPolicyError::ProjectNotFound { .. }) => {
                    rows.push(skipped_row(
                        project_id,
                        CloudTelemetryFidelity::Metered,
                        window_from,
                        window_to,
                        BulkSkipReason::ProjectNotFound,
                    ));
                    continue;
                }
                Err(error) => return Err(Problem::from(error)),
            },
            // No policy resolver wired means every project mirrors at
            // `metered`, which is exactly the state a Cloud-primary switch is
            // not allowed to change on the operator's behalf.
            None => crate::services::CloudTelemetryPolicy::metered(),
        };

        // The same gate the worker applies, applied here so the operator sees
        // the skip and its fix *before* confirming rather than discovering it
        // in a finished job's failure list.
        if !policy.fidelity.is_queryable() {
            rows.push(skipped_row(
                project_id,
                policy.fidelity,
                window_from,
                window_to,
                BulkSkipReason::FidelityNotQueryable,
            ));
            continue;
        }

        let estimate = estimate_backfill(
            source.as_ref(),
            cloud_link.as_ref(),
            &policy,
            project_id,
            window_from,
            window_to,
        )
        .await?;

        rows.push(BulkActivationProjectEstimateResponse {
            project_id,
            eligible: true,
            skip_reason: None,
            skip_detail: None,
            setup_path: None,
            fidelity: estimate.fidelity,
            window_from,
            window_to,
            estimated_spans: estimate.spans,
            estimated_bytes: estimate.estimated_metered_bytes,
            sampled_spans: estimate.sampled_spans,
            average_span_bytes: estimate.average_span_bytes,
        });
        plan.push(BulkJobProjectPlan {
            project_id,
            window_from,
            window_to,
            estimated_spans: estimate.spans,
            estimated_bytes: estimate.estimated_metered_bytes,
        });
    }

    rows.sort_by_key(|row| row.project_id);

    let eligible_projects = plan.len();
    let skipped_projects = rows.len() - eligible_projects;
    let estimated_spans = plan.iter().fold(0u64, |total, item| {
        total.saturating_add(item.estimated_spans)
    });
    let estimated_bytes = plan.iter().fold(0u64, |total, item| {
        total.saturating_add(item.estimated_bytes)
    });

    // Minting is skipped when nothing is eligible: there is no bill, and a
    // token that would be refused on submit is a dead end with no explanation.
    let minted = if plan.is_empty() {
        None
    } else {
        Some(mint_plan_token(&state.plan_signing_key, &plan, now)?)
    };

    let active_batch_id = state
        .bulk_activation
        .active_job()
        .await?
        .map(|detail| detail.job.id);

    Ok(Json(BulkActivationEstimateResponse {
        configured: true,
        reason: None,
        setup_path: None,
        window_from,
        window_to,
        total_projects: rows.len(),
        projects: rows,
        eligible_projects,
        skipped_projects,
        estimated_spans,
        estimated_bytes,
        plan_token: minted.as_ref().map(|minted| minted.token.clone()),
        plan_hash: minted.as_ref().map(|minted| minted.plan_hash.clone()),
        plan_expires_at: minted.as_ref().map(|minted| minted.expires_at),
        active_batch_id,
    }))
}

/// Queue the activation that was quoted.
#[utoipa::path(
    tag = "OTel",
    post,
    path = "/otel/cloud-telemetry/bulk-jobs",
    request_body = CreateBulkActivationJobRequest,
    responses(
        (status = 202, description = "The activation was queued", body = BulkActivationJobResponse),
        (status = 400, description = "The plan token is invalid, altered or expired", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "An activation is already running; the response carries its batch_id", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_bulk_activation_job(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Json(request): Json<CreateBulkActivationJobRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);
    instance_admin_guard(&auth)?;

    let now = chrono::Utc::now();
    let plan = verify_plan_token(&state.plan_signing_key, &request.plan_token, now)?;

    let detail = state
        .bulk_activation
        .enqueue_job(EnqueueBulkJobRequest {
            trigger: BulkJobTrigger::Operator,
            // `user_id_opt`, not `user_id`: the latter collapses "no user
            // behind this principal" to `0`, and the column is a foreign key
            // into `users`. A dangling 0 would fail the insert with a
            // constraint name instead of attributing the spend to nobody,
            // which is what the nullable column already means.
            requested_by_user_id: auth.user_id_opt(),
            plan_hash: Some(plan.plan_hash.clone()),
            projects: plan.projects.clone(),
        })
        .await?;

    let audit = crate::handlers::audit::CloudTelemetryBulkActivationStartedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        batch_id: detail.job.id.to_string(),
        trigger: detail.job.trigger.to_string(),
        plan_hash: plan.plan_hash,
        project_ids: plan.projects.iter().map(|item| item.project_id).collect(),
        project_count: plan.projects.len(),
        estimated_spans: detail.job.estimated_spans,
        estimated_bytes: detail.job.estimated_bytes,
        window_from: plan
            .projects
            .iter()
            .map(|item| item.window_from)
            .min()
            .unwrap_or(now)
            .to_rfc3339(),
        window_to: plan
            .projects
            .iter()
            .map(|item| item.window_to)
            .max()
            .unwrap_or(now)
            .to_rfc3339(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    // 202, not 201: the job is queued, and the thing the caller cares about —
    // the history actually being on Cloud — has not happened yet.
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(build_job_response(&detail, now)),
    ))
}

/// One activation job, with per-project rows and an ETA.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/cloud-telemetry/bulk-jobs/{batch_id}",
    params(
        ("batch_id" = String, Path, description = "Activation job id (UUID)"),
    ),
    responses(
        (status = 200, description = "The activation job", body = BulkActivationJobResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "No such activation job", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_bulk_activation_job(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(batch_id): Path<Uuid>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    instance_admin_guard(&auth)?;

    let detail = state.bulk_activation.job_detail(batch_id).await?;
    Ok(Json(build_job_response(&detail, chrono::Utc::now())))
}

/// The activation currently pending or running, or `null`.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/cloud-telemetry/bulk-jobs/current",
    responses(
        (status = 200, description = "The active activation job, or null when none is running", body = Option<BulkActivationJobResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_current_bulk_activation_job(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    instance_admin_guard(&auth)?;

    // 200 with `null`, never 404. "No activation is running" is the ordinary
    // state of a healthy instance, and a 404 would make the Console unable to
    // tell it apart from "this build has no activation feature".
    let now = chrono::Utc::now();
    let active = state
        .bulk_activation
        .active_job()
        .await?
        .map(|detail| build_job_response(&detail, now));
    Ok(Json(active))
}

/// Ask an activation to stop at the next chunk boundary.
#[utoipa::path(
    tag = "OTel",
    post,
    path = "/otel/cloud-telemetry/bulk-jobs/{batch_id}/cancel",
    params(
        ("batch_id" = String, Path, description = "Activation job id (UUID)"),
    ),
    responses(
        (status = 200, description = "The cancellation was recorded, or the job had already stopped", body = BulkActivationJobResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "No such activation job", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_bulk_activation_job(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path(batch_id): Path<Uuid>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelWrite);
    instance_admin_guard(&auth)?;

    // Idempotent by design: a job that finished a second before the click is a
    // race a UI loses regularly, and failing it would be noise on a control
    // whose whole job is to stop something.
    let before = state.bulk_activation.job(batch_id).await?;
    state.bulk_activation.request_cancel(batch_id).await?;

    let audit = crate::handlers::audit::CloudTelemetryBulkActivationCancelledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        batch_id: batch_id.to_string(),
        status_at_request: before.status.to_string(),
        spans_shipped: before.spans_shipped,
        bytes_shipped: before.bytes_shipped,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let detail = state.bulk_activation.job_detail(batch_id).await?;
    Ok(Json(build_job_response(&detail, chrono::Utc::now())))
}

// ── Shared assembly ────────────────────────────────────────────────────────

/// Confine these endpoints to an instance operator.
///
/// `permission_guard!` proves the caller holds `otel:write`; it does not prove
/// they run this instance. Every one of these routes is instance-wide by
/// construction — the scope is "these projects", chosen in the body, so there
/// is no path parameter for `project_scope_guard!` to narrow. A project-scoped
/// token or a per-project role must therefore not reach them at all, or it
/// could switch and bill projects it has no relationship with.
fn instance_admin_guard(auth: &temps_auth::AuthContext) -> Result<(), Problem> {
    if auth.is_instance_admin() {
        return Ok(());
    }
    Err(ErrorBuilder::new(axum::http::StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/insufficient-permissions")
        .title("Instance Administrator Required")
        .detail(
            "Bulk Temps Cloud telemetry activation switches every named project's spans away \
             from this instance and ships their history at the instance's expense, so it is \
             restricted to an instance administrator.",
        )
        .value("required_role", temps_auth::Role::PlatformAdmin.to_string())
        .value("user_role", auth.effective_role.to_string())
        .build())
}

fn unlinked_reason() -> String {
    format!(
        "This instance is not linked to Temps Cloud, so there is nowhere to activate projects \
         to. Linking it lets every project's spans be written to Temps Cloud and its existing \
         local history shipped there in one job, with progress and an ETA. Link it at \
         {CLOUD_SETUP_PATH}."
    )
}

/// The window to quote, defaulted and validated.
///
/// The default is the whole of what local retention can still be holding, which
/// is the ADR's "everything local storage holds" — anything older has already
/// been dropped by the retention policy, and quoting it would promise history
/// that no longer exists.
fn resolve_estimate_window(
    request: &EstimateBulkActivationRequest,
    now: DBDateTime,
) -> Result<(DBDateTime, DBDateTime), Problem> {
    let window_to = request.window_to.unwrap_or(now);
    let window_from = request
        .window_from
        .unwrap_or_else(|| now - chrono::Duration::days(local_retention_days() as i64));

    if window_to < window_from {
        return Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Invalid Activation Window")
            .with_detail(format!(
                "The window [{}, {}] ends before it starts, so it could never ship anything.",
                window_from.to_rfc3339(),
                window_to.to_rfc3339()
            )));
    }

    Ok((window_from, window_to))
}

/// What a request asked to be quoted, once it is known to make sense.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EstimateScope {
    /// Every project still writing its spans to this instance.
    AllEligible,
    /// Exactly these, deduplicated and in ascending order.
    Explicit(Vec<i32>),
}

/// Check the two mutually exclusive scopes before anything else happens.
///
/// Pure, and called **before** the Cloud-link check: "you named no projects" is
/// true and actionable whether or not this instance is linked, and reporting
/// "Temps Cloud is not connected" to a request that was malformed anyway would
/// send the operator to fix the wrong thing.
pub(crate) fn validate_scope(
    request: &EstimateBulkActivationRequest,
) -> Result<EstimateScope, Problem> {
    let explicit: Vec<i32> = request.project_ids.clone().unwrap_or_default();

    match (request.all_eligible_projects, explicit.is_empty()) {
        (true, false) => Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("Ambiguous Activation Scope")
            .with_detail(
                "This request asks for every eligible project *and* names specific ones. Send \
                 one or the other, so what was quoted is unambiguous.",
            )),
        (false, true) => Err(problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
            .with_title("No Activation Scope Given")
            .with_detail(
                "Name the projects to activate, or set `all_eligible_projects` to quote every \
                 project still writing its spans to this instance. This endpoint never defaults \
                 to \"everything\": a typo must not be able to quote — and then bill — an \
                 activation nobody asked for.",
            )),
        (true, true) => Ok(EstimateScope::AllEligible),
        (false, false) => {
            if explicit.len() > MAX_ESTIMATE_PROJECTS {
                return Err(too_many_projects(explicit.len(), false));
            }
            // Deduplicated and ordered here rather than left to the service:
            // the same project twice would be quoted twice and would bill the
            // same window twice if the duplicate check ever moved.
            Ok(EstimateScope::Explicit(
                explicit
                    .into_iter()
                    .collect::<BTreeSet<i32>>()
                    .into_iter()
                    .collect(),
            ))
        }
    }
}

/// Turn a validated scope into the project ids to quote.
async fn resolve_candidates(
    state: &OtelAppState,
    scope: EstimateScope,
) -> Result<Vec<i32>, Problem> {
    match scope {
        EstimateScope::Explicit(ids) => Ok(ids),
        EstimateScope::AllEligible => {
            let all = state.telemetry_write_modes.local_mode_project_ids().await?;
            // An instance larger than one plan can carry gets told how to
            // proceed, not just that it cannot. "Too many" with no next step is
            // a dead end for an operator with nobody to ask.
            if all.len() > MAX_ESTIMATE_PROJECTS {
                return Err(too_many_projects(all.len(), true));
            }
            Ok(all)
        }
    }
}

/// One plan cannot carry this many projects — and here is what to do instead.
fn too_many_projects(count: usize, from_all: bool) -> Problem {
    let advice = if from_all {
        "This instance has more projects still storing spans locally than one activation plan \
         can carry. Activate them in batches by naming project ids explicitly with \
         `project_ids`; each batch runs, and finishes, on its own."
    } else {
        "Split the activation into smaller batches; each one runs, and finishes, on its own."
    };
    ErrorBuilder::new(axum::http::StatusCode::BAD_REQUEST)
        .title("Too Many Projects For One Activation")
        .detail(format!(
            "An activation plan may name at most {MAX_ESTIMATE_PROJECTS} project(s); this one \
             names {count}. {advice}"
        ))
        .value("max_projects", MAX_ESTIMATE_PROJECTS)
        .value("requested_projects", count)
        .build()
}

fn skipped_row(
    project_id: i32,
    fidelity: CloudTelemetryFidelity,
    window_from: DBDateTime,
    window_to: DBDateTime,
    reason: BulkSkipReason,
) -> BulkActivationProjectEstimateResponse {
    BulkActivationProjectEstimateResponse {
        project_id,
        eligible: false,
        skip_reason: Some(reason.as_str().to_string()),
        skip_detail: Some(skip_detail(project_id, reason)),
        setup_path: reason.setup_path(project_id),
        fidelity,
        window_from,
        window_to,
        estimated_spans: 0,
        estimated_bytes: 0,
        sampled_spans: 0,
        average_span_bytes: 0.0,
    }
}

/// The sentence shown for a skip, written server-side.
///
/// Sent rather than left to the client so the same words reach the Console, the
/// CLI and a log line — a self-hosted operator reading
/// `fidelity_not_queryable` in a table has nobody to ask what it means.
pub(crate) fn skip_detail(project_id: i32, reason: BulkSkipReason) -> String {
    match reason {
        BulkSkipReason::FidelityNotQueryable => format!(
            "Project {project_id}'s Cloud telemetry fidelity is not `queryable`, so switching it \
             would store pseudonymised placeholders in Temps Cloud and nothing on this instance \
             — its traces would exist nowhere. Raising fidelity costs money and changes what \
             leaves this instance, so this activation will not do it for you. Raise it, then \
             re-run the activation for this project."
        ),
        BulkSkipReason::ProjectNotFound => format!(
            "Project {project_id} does not exist on this instance, or was deleted. Nothing was \
             switched and nothing was shipped for it."
        ),
    }
}

/// Split a stored `"<code>: <sentence>"` abort reason back into its two halves.
///
/// The worker stores both in one column so a single read answers "what happened
/// and what do I do", but a client needs the code to branch on and the sentence
/// to show. A row written without the prefix (an older build, a manual
/// `UPDATE`) yields no code and the whole string as the detail, rather than
/// silently mislabelling its first word as a code.
pub(crate) fn split_abort_reason(stored: &str) -> (Option<String>, String) {
    match stored.split_once(": ") {
        Some((code, detail))
            if !code.is_empty()
                && !code.contains(' ')
                && code.chars().all(|c| c.is_ascii_lowercase() || c == '_') =>
        {
            (Some(code.to_string()), detail.to_string())
        }
        _ => (None, stored.to_string()),
    }
}

/// The job's own observed throughput, in spans per second.
///
/// `None` before the first chunk acknowledges: there is no measured rate yet,
/// and this instance must not invent one.
pub(crate) fn observed_spans_per_sec(job: &BulkJob, now: DBDateTime) -> Option<f64> {
    let started_at = job.started_at?;
    if job.spans_shipped <= 0 {
        return None;
    }
    let elapsed = (now - started_at).num_milliseconds();
    if elapsed <= 0 {
        return None;
    }
    Some(job.spans_shipped as f64 * 1000.0 / elapsed as f64)
}

/// Seconds remaining, and how much of that this instance actually knows.
///
/// `remaining_spans / observed_throughput`. A terminal job is `Finished`; a job
/// with no observed throughput, or no total to measure against, is `Estimating`
/// with a `None` ETA so the client renders "estimating…" rather than a number
/// derived from a division by something that is not yet a rate.
pub(crate) fn estimate_eta(
    job: &BulkJob,
    now: DBDateTime,
) -> (Option<i64>, BulkActivationEtaState) {
    if job.status.is_terminal() {
        return (None, BulkActivationEtaState::Finished);
    }
    if job.estimated_spans <= 0 {
        return (None, BulkActivationEtaState::Estimating);
    }
    let Some(throughput) = observed_spans_per_sec(job, now) else {
        return (None, BulkActivationEtaState::Estimating);
    };
    if throughput <= 0.0 {
        return (None, BulkActivationEtaState::Estimating);
    }

    let remaining = (job.estimated_spans - job.spans_shipped).max(0) as f64;
    let seconds = (remaining / throughput).ceil();
    // Guard the cast: a throughput of a fraction of a span per second over a
    // large remainder can exceed i64 as an f64, and a wrapped negative would
    // render as a countdown that has already finished.
    if !seconds.is_finite() || seconds > i64::MAX as f64 {
        return (None, BulkActivationEtaState::Estimating);
    }
    (Some(seconds as i64), BulkActivationEtaState::Known)
}

/// `shipped / total` as a percentage, or `None` when there is no total.
///
/// Shared with the per-project rows so a job at 0 of 0 reads as "nothing to
/// send" in both places rather than as "stuck at 0%".
fn percent(shipped: i64, total: i64) -> Option<f64> {
    crate::services::cloud_backfill_progress::percent_complete(shipped, total)
}

fn build_job_response(detail: &BulkJobDetail, now: DBDateTime) -> BulkActivationJobResponse {
    let job = &detail.job;
    let (eta_seconds, eta_state) = estimate_eta(job, now);
    let (abort_reason, abort_detail) = match job.abort_reason.as_deref() {
        Some(stored) => {
            let (code, detail) = split_abort_reason(stored);
            (code, Some(detail))
        }
        None => (None, None),
    };

    BulkActivationJobResponse {
        batch_id: job.id,
        trigger: job.trigger,
        status: job.status,
        requested_by_user_id: job.requested_by_user_id,
        plan_hash: job.plan_hash.clone(),
        estimated_spans: job.estimated_spans,
        estimated_bytes: job.estimated_bytes,
        spans_shipped: job.spans_shipped,
        bytes_shipped: job.bytes_shipped,
        percent_complete: percent(job.spans_shipped, job.estimated_spans),
        eta_seconds,
        eta_state,
        observed_spans_per_sec: observed_spans_per_sec(job, now),
        current_project_id: detail
            .projects
            .iter()
            .find(|project| {
                matches!(
                    project.status,
                    BulkJobProjectStatus::Switching | BulkJobProjectStatus::Backfilling
                )
            })
            .map(|project| project.project_id),
        projects_total: detail.projects.len(),
        projects_pending: detail.pending_projects(),
        projects_done: count(detail, BulkJobProjectStatus::Done),
        projects_failed: count(detail, BulkJobProjectStatus::Failed),
        projects_skipped: count(detail, BulkJobProjectStatus::Skipped),
        cancel_requested: job.cancel_requested_at.is_some(),
        cancel_requested_at: job.cancel_requested_at,
        abort_reason,
        abort_detail,
        created_at: job.created_at,
        started_at: job.started_at,
        completed_at: job.completed_at,
        projects: detail
            .projects
            .iter()
            .map(build_job_project_response)
            .collect(),
    }
}

fn count(detail: &BulkJobDetail, status: BulkJobProjectStatus) -> usize {
    detail
        .projects
        .iter()
        .filter(|project| project.status == status)
        .count()
}

fn build_job_project_response(project: &BulkJobProject) -> BulkActivationJobProjectResponse {
    let reason = project.skip_reason.as_deref().and_then(parse_skip_reason);

    BulkActivationJobProjectResponse {
        project_id: project.project_id,
        status: project.status,
        skip_reason: project.skip_reason.clone(),
        skip_detail: reason.map(|reason| skip_detail(project.project_id, reason)),
        setup_path: reason.and_then(|reason| reason.setup_path(project.project_id)),
        window_from: project.window_from,
        window_to: project.window_to,
        estimated_spans: project.estimated_spans,
        estimated_bytes: project.estimated_bytes,
        spans_shipped: project.spans_shipped,
        bytes_shipped: project.bytes_shipped,
        percent_complete: percent(project.spans_shipped, project.estimated_spans),
        // Only while the row is actually failed. A `done` row keeps whatever
        // the previous attempt wrote, and showing it would make a healthy
        // project read as broken.
        last_error: match project.status {
            BulkJobProjectStatus::Failed => project
                .last_error
                .as_deref()
                .map(crate::services::cloud_backfill_progress::truncate_failure_reason),
            _ => None,
        },
        started_at: project.started_at,
        completed_at: project.completed_at,
    }
}

fn parse_skip_reason(stored: &str) -> Option<BulkSkipReason> {
    match stored {
        "fidelity_not_queryable" => Some(BulkSkipReason::FidelityNotQueryable),
        "project_not_found" => Some(BulkSkipReason::ProjectNotFound),
        // An unrecognised token (an older build, a manual `UPDATE`) is still
        // surfaced verbatim as `skip_reason`; it just gets no sentence and no
        // link, which is honest rather than a guessed one that 404s.
        _ => None,
    }
}

impl From<CloudPolicyError> for Problem {
    fn from(error: CloudPolicyError) -> Self {
        match error {
            CloudPolicyError::ProjectNotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            CloudPolicyError::Lookup { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Could Not Read Cloud Telemetry Settings")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DBDateTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc)
    }

    fn job(status: BulkJobStatus) -> BulkJob {
        BulkJob {
            id: Uuid::nil(),
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: Some(1),
            status,
            estimated_spans: 10_000,
            estimated_bytes: 2_500_000,
            spans_shipped: 0,
            bytes_shipped: 0,
            plan_hash: Some("deadbeef".to_string()),
            created_at: at("2026-09-01T12:00:00Z"),
            started_at: None,
            completed_at: None,
            cancel_requested_at: None,
            abort_reason: None,
        }
    }

    fn project(project_id: i32, status: BulkJobProjectStatus) -> BulkJobProject {
        BulkJobProject {
            id: project_id as i64,
            job_id: Uuid::nil(),
            project_id,
            status,
            skip_reason: None,
            window_from: at("2026-08-01T00:00:00Z"),
            window_to: at("2026-09-01T00:00:00Z"),
            estimated_spans: 5_000,
            estimated_bytes: 1_250_000,
            spans_shipped: 0,
            bytes_shipped: 0,
            resume_start_time: None,
            resume_row_id: None,
            resume_span_id: None,
            last_error: None,
            started_at: None,
            completed_at: None,
        }
    }

    // ── ETA: "estimating…" is a state, not a number ──────────────────────

    #[test]
    fn a_job_that_has_not_started_reports_estimating_rather_than_a_number() {
        // A countdown invented before any chunk acknowledged would be a number
        // this instance cannot justify, on a screen a customer is watching
        // because they are spending money.
        let (eta, state) = estimate_eta(&job(BulkJobStatus::Pending), at("2026-09-01T12:00:05Z"));
        assert_eq!(eta, None);
        assert_eq!(state, BulkActivationEtaState::Estimating);
    }

    #[test]
    fn a_started_job_with_nothing_shipped_yet_is_still_estimating() {
        let mut running = job(BulkJobStatus::Running);
        running.started_at = Some(at("2026-09-01T12:00:00Z"));

        let (eta, state) = estimate_eta(&running, at("2026-09-01T12:00:30Z"));
        assert_eq!(
            eta, None,
            "zero shipped over thirty seconds is not a rate, it is no data"
        );
        assert_eq!(state, BulkActivationEtaState::Estimating);
    }

    #[test]
    fn an_eta_is_remaining_spans_over_the_observed_rate() {
        // 2,000 of 10,000 spans in 100 seconds is 20 spans/second, so the
        // remaining 8,000 take 400 seconds.
        let mut running = job(BulkJobStatus::Running);
        running.started_at = Some(at("2026-09-01T12:00:00Z"));
        running.spans_shipped = 2_000;

        let now = at("2026-09-01T12:01:40Z");
        assert_eq!(observed_spans_per_sec(&running, now), Some(20.0));
        assert_eq!(
            estimate_eta(&running, now),
            (Some(400), BulkActivationEtaState::Known)
        );
    }

    #[test]
    fn a_job_that_overshot_its_estimate_reports_zero_rather_than_a_negative_eta() {
        // The estimate extrapolates from a 1,000-span sample, so it can be
        // wrong. A negative countdown would render as a time in the past.
        let mut running = job(BulkJobStatus::Running);
        running.started_at = Some(at("2026-09-01T12:00:00Z"));
        running.spans_shipped = 12_000;

        let (eta, state) = estimate_eta(&running, at("2026-09-01T12:01:40Z"));
        assert_eq!(eta, Some(0));
        assert_eq!(state, BulkActivationEtaState::Known);
    }

    #[test]
    fn a_job_with_no_estimated_total_never_claims_to_know_an_eta() {
        let mut running = job(BulkJobStatus::Running);
        running.estimated_spans = 0;
        running.started_at = Some(at("2026-09-01T12:00:00Z"));
        running.spans_shipped = 500;

        assert_eq!(
            estimate_eta(&running, at("2026-09-01T12:00:10Z")),
            (None, BulkActivationEtaState::Estimating)
        );
    }

    #[test]
    fn every_terminal_status_reports_finished_and_no_eta() {
        for status in [
            BulkJobStatus::Completed,
            BulkJobStatus::CompletedWithFailures,
            BulkJobStatus::Aborted,
            BulkJobStatus::Cancelled,
        ] {
            let mut done = job(status);
            done.started_at = Some(at("2026-09-01T12:00:00Z"));
            done.spans_shipped = 4_000;

            let (eta, state) = estimate_eta(&done, at("2026-09-01T12:01:40Z"));
            assert_eq!(eta, None, "{status} must not carry a countdown");
            assert_eq!(state, BulkActivationEtaState::Finished, "{status}");
        }
    }

    // ── Job response shape ───────────────────────────────────────────────

    #[test]
    fn the_response_names_the_project_currently_being_worked_on() {
        // "Which project is it on" is the first thing an operator watching a
        // forty-project activation wants, and it is not derivable from the
        // totals.
        let detail = BulkJobDetail {
            job: job(BulkJobStatus::Running),
            projects: vec![
                project(1, BulkJobProjectStatus::Done),
                project(4, BulkJobProjectStatus::Backfilling),
                project(9, BulkJobProjectStatus::Pending),
            ],
        };

        let response = build_job_response(&detail, at("2026-09-01T12:01:40Z"));
        assert_eq!(response.current_project_id, Some(4));
        assert_eq!(response.projects_total, 3);
        assert_eq!(response.projects_done, 1);
        assert_eq!(response.projects_pending, 2);
    }

    #[test]
    fn a_skipped_project_carries_its_reason_its_sentence_and_the_page_that_fixes_it() {
        // A skip an operator cannot act on is indistinguishable from a bug.
        let mut skipped = project(7, BulkJobProjectStatus::Skipped);
        skipped.skip_reason = Some("fidelity_not_queryable".to_string());
        let detail = BulkJobDetail {
            job: job(BulkJobStatus::CompletedWithFailures),
            projects: vec![skipped],
        };

        let response = build_job_response(&detail, at("2026-09-01T12:01:40Z"));
        let row = &response.projects[0];
        assert_eq!(row.skip_reason.as_deref(), Some("fidelity_not_queryable"));
        assert!(row
            .skip_detail
            .as_deref()
            .unwrap_or_default()
            .contains("queryable"));
        assert_eq!(
            row.setup_path.as_deref(),
            Some("/projects/7/settings/telemetry")
        );
    }

    #[test]
    fn an_unknown_skip_token_is_shown_verbatim_without_an_invented_link() {
        let mut skipped = project(7, BulkJobProjectStatus::Skipped);
        skipped.skip_reason = Some("written_by_a_newer_build".to_string());
        let detail = BulkJobDetail {
            job: job(BulkJobStatus::Completed),
            projects: vec![skipped],
        };

        let row = &build_job_response(&detail, at("2026-09-01T12:01:40Z")).projects[0];
        assert_eq!(row.skip_reason.as_deref(), Some("written_by_a_newer_build"));
        assert_eq!(row.skip_detail, None);
        assert_eq!(
            row.setup_path, None,
            "a guessed link that 404s is worse than none"
        );
    }

    #[test]
    fn a_done_project_never_shows_a_previous_attempts_error() {
        let mut done = project(7, BulkJobProjectStatus::Done);
        done.last_error = Some("stale failure from the previous attempt".to_string());
        let mut failed = project(9, BulkJobProjectStatus::Failed);
        failed.last_error = Some("Temps Cloud did not accept a batch".to_string());

        let detail = BulkJobDetail {
            job: job(BulkJobStatus::CompletedWithFailures),
            projects: vec![done, failed],
        };
        let response = build_job_response(&detail, at("2026-09-01T12:01:40Z"));

        assert_eq!(response.projects[0].last_error, None);
        assert_eq!(
            response.projects[1].last_error.as_deref(),
            Some("Temps Cloud did not accept a batch")
        );
    }

    #[test]
    fn an_empty_window_reports_no_percentage_rather_than_zero() {
        let mut empty = job(BulkJobStatus::Running);
        empty.estimated_spans = 0;
        let detail = BulkJobDetail {
            job: empty,
            projects: Vec::new(),
        };

        assert_eq!(
            build_job_response(&detail, at("2026-09-01T12:01:40Z")).percent_complete,
            None,
            "0% would read as stuck; the job simply has nothing to send"
        );
    }

    #[test]
    fn a_cancel_is_visible_before_the_worker_honours_it() {
        // The worker acts at the next chunk boundary, which can be minutes
        // away. Without this the UI would keep offering a Cancel button that
        // looks like it did nothing.
        let mut running = job(BulkJobStatus::Running);
        running.cancel_requested_at = Some(at("2026-09-01T12:00:30Z"));
        let detail = BulkJobDetail {
            job: running,
            projects: vec![project(1, BulkJobProjectStatus::Backfilling)],
        };

        let response = build_job_response(&detail, at("2026-09-01T12:01:40Z"));
        assert!(response.cancel_requested);
        assert_eq!(response.status, BulkJobStatus::Running);
    }

    // ── Abort reasons ────────────────────────────────────────────────────

    #[test]
    fn an_abort_reason_splits_into_a_code_to_branch_on_and_a_sentence_to_show() {
        let detail = BulkJobDetail {
            job: {
                let mut aborted = job(BulkJobStatus::Aborted);
                aborted.abort_reason = Some(
                    "not_linked: This instance is no longer linked to Temps Cloud, so there is \
                     nowhere to activate projects to."
                        .to_string(),
                );
                aborted
            },
            projects: Vec::new(),
        };

        let response = build_job_response(&detail, at("2026-09-01T12:01:40Z"));
        assert_eq!(response.abort_reason.as_deref(), Some("not_linked"));
        assert!(response
            .abort_detail
            .as_deref()
            .unwrap_or_default()
            .starts_with("This instance is no longer linked"));
    }

    #[test]
    fn an_abort_reason_with_no_code_prefix_is_shown_whole_rather_than_mislabelled() {
        let (code, detail) = split_abort_reason("Something went wrong: and here is why");
        assert_eq!(code, None);
        assert_eq!(detail, "Something went wrong: and here is why");
    }

    // ── Error mapping ────────────────────────────────────────────────────

    #[test]
    fn an_already_running_job_is_a_conflict_carrying_the_id_to_watch() {
        // The caller's next move is to watch or cancel that job. A bare 409
        // with prose leaves a CLI or a UI with nothing to act on.
        let job_id = Uuid::new_v4();
        let problem = Problem::from(CloudBulkActivationError::JobAlreadyActive {
            job_id,
            status: BulkJobStatus::Running,
        });

        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        let body = format!("{problem:?}");
        assert!(body.contains(&job_id.to_string()), "{body}");
        assert!(body.contains("batch_id"), "{body}");
    }

    #[test]
    fn a_missing_job_is_a_404_not_a_conflict() {
        let problem = Problem::from(CloudBulkActivationError::JobNotFound {
            job_id: Uuid::nil(),
        });
        assert_eq!(problem.status_code, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn every_plan_token_refusal_is_a_400_that_points_at_the_estimate_endpoint() {
        // None of these is a permission problem, and rendering one as 403 would
        // send the operator to look at their role instead of clicking the
        // button that fixes it.
        for error in [
            PlanTokenError::SignatureMismatch,
            PlanTokenError::Expired {
                expired_at: "2026-09-01T12:00:00+00:00".into(),
                ttl_seconds: 900,
            },
            PlanTokenError::Malformed {
                reason: "it has no `.`".into(),
            },
            PlanTokenError::UnsupportedVersion {
                version: "v2".into(),
            },
        ] {
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
            let body = format!("{problem:?}");
            assert!(body.contains("bulk-jobs/estimate"), "{body}");
        }
    }

    #[test]
    fn a_link_level_backfill_refusal_is_a_conflict_with_a_setup_path() {
        let problem = Problem::from(CloudBackfillError::NotLinked { project_id: 7 });
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        assert!(format!("{problem:?}").contains(CLOUD_SETUP_PATH));
    }

    #[test]
    fn a_read_failure_while_estimating_is_a_500_and_names_the_project() {
        let problem = Problem::from(CloudBackfillError::ClickHouse {
            project_id: 7,
            from: "2026-08-01T00:00:00Z".into(),
            to: "2026-09-01T00:00:00Z".into(),
            reason: "connection reset".into(),
        });
        assert_eq!(
            problem.status_code,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(format!("{problem:?}").contains("project 7"));
    }

    // ── Scope resolution ─────────────────────────────────────────────────

    #[test]
    fn the_default_window_covers_what_local_retention_can_still_hold() {
        // Quoting further back would promise history the retention policy has
        // already dropped.
        let now = at("2026-09-01T12:00:00Z");
        let (from, to) = resolve_estimate_window(&EstimateBulkActivationRequest::default(), now)
            .expect("the default window is always valid");

        assert_eq!(to, now);
        assert_eq!(
            from,
            now - chrono::Duration::days(local_retention_days() as i64)
        );
    }

    #[test]
    fn a_backwards_window_is_refused_before_anything_is_counted() {
        let now = at("2026-09-01T12:00:00Z");
        let request = EstimateBulkActivationRequest {
            window_from: Some(at("2026-09-02T00:00:00Z")),
            window_to: Some(at("2026-09-01T00:00:00Z")),
            ..Default::default()
        };

        let problem = resolve_estimate_window(&request, now).expect_err("must refuse");
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_request_naming_no_projects_is_refused_rather_than_defaulting_to_everything() {
        // Defaulting an omitted scope to "everything" would let a typo quote —
        // and then bill — an activation nobody asked for.
        let problem = validate_scope(&EstimateBulkActivationRequest::default())
            .expect_err("an empty scope must be refused");
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
        assert!(format!("{problem:?}").contains("never defaults"));
    }

    #[test]
    fn asking_for_all_and_for_specific_projects_at_once_is_refused() {
        let problem = validate_scope(&EstimateBulkActivationRequest {
            all_eligible_projects: true,
            project_ids: Some(vec![1]),
            ..Default::default()
        })
        .expect_err("an ambiguous scope must be refused");
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn an_explicit_scope_is_deduplicated_and_ordered() {
        // The same project twice would be quoted twice, and would bill the same
        // window twice if the service's duplicate check ever moved.
        assert_eq!(
            validate_scope(&EstimateBulkActivationRequest {
                project_ids: Some(vec![9, 4, 9, 1]),
                ..Default::default()
            })
            .expect("a valid scope"),
            EstimateScope::Explicit(vec![1, 4, 9])
        );
    }

    #[test]
    fn a_malformed_scope_is_refused_even_on_an_unlinked_instance() {
        // Scope validity is a property of the request, not of the link. This is
        // checked before the Cloud-link branch precisely so a malformed request
        // is not answered with "Temps Cloud is not connected", which would send
        // the operator to fix the one thing that is not wrong.
        assert!(validate_scope(&EstimateBulkActivationRequest::default()).is_err());
        assert!(validate_scope(&EstimateBulkActivationRequest {
            all_eligible_projects: true,
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn an_oversized_plan_says_how_to_proceed_rather_than_only_that_it_cannot() {
        // "Too many" with no next step is a dead end for an operator with
        // nobody to ask, and the two ways to hit it need different advice.
        let from_all = too_many_projects(MAX_ESTIMATE_PROJECTS + 1, true);
        assert_eq!(from_all.status_code, axum::http::StatusCode::BAD_REQUEST);
        let body = format!("{from_all:?}");
        assert!(body.contains("naming project ids explicitly"), "{body}");
        assert!(body.contains("max_projects"), "{body}");

        let explicit = too_many_projects(MAX_ESTIMATE_PROJECTS + 1, false);
        assert!(format!("{explicit:?}").contains("smaller batches"));
    }

    #[test]
    fn a_caller_without_the_platform_admin_role_is_refused() {
        // `otel:write` proves the caller may change telemetry settings; it does
        // not prove they run the instance whose money this spends.
        let auth = temps_auth::AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deploy-token".to_string(),
            Vec::new(),
        );

        let problem = instance_admin_guard(&auth).expect_err("must refuse");
        assert_eq!(problem.status_code, axum::http::StatusCode::FORBIDDEN);
        assert!(format!("{problem:?}").contains("required_role"));
    }
}
