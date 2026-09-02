// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project telemetry write mode and fidelity, plus the instance-level
//! aggregate (ADR-041 §9).
//!
//! # Why these always answer
//!
//! The write-mode control sits beside the fidelity control in project settings
//! and **renders even when Cloud is not linked**, in an onboarding state: what
//! it would do, what is missing, and where to fix it. That is only possible if
//! the endpoint answers on an unlinked instance rather than 404-ing, so
//! [`ProjectCloudTelemetryResponse`] carries `configured` / `reason` /
//! `setup_path` in every state — the same contract `GET /cloud/capability`
//! already uses, so a client can tell "not built" from "not set up" without
//! inferring it from an error.
//!
//! The instance aggregate is the page an operator reads before deciding whether
//! they can decommission a local span store, so it leads with
//! `local_span_store_required` and its specific reason. A partial cutover
//! yields *zero* resource win, and operators will reasonably believe otherwise
//! unless the signal is prominent and derived rather than implied.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;

use crate::services::telemetry_write_mode::{
    CloudLinkSnapshot, ProjectTelemetryWriteSettings, TelemetryWriteModeError, CLOUD_SETUP_PATH,
};
use crate::OtelAppState;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, ProblemDetails, RequestMetadata};
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode;
use temps_entities::project_telemetry_write_intervals::TelemetryWriteIntervalReason;

/// How far back the gap-window and interval lists look on the aggregate view.
const HISTORY_DAYS: i64 = 30;
/// Bound on any list returned here. Matches the workspace pagination ceiling.
const MAX_ITEMS: u64 = 100;

// ── Error mapping ──────────────────────────────────────────────────────────

impl From<TelemetryWriteModeError> for Problem {
    fn from(error: TelemetryWriteModeError) -> Self {
        use temps_core::error_builder::ErrorBuilder;
        match error {
            TelemetryWriteModeError::ProjectNotFound { .. } => {
                problemdetails::new(axum::http::StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }

            // The four gate refusals are 409 Conflict, not 400: the request is
            // well-formed and the feature exists — the instance is simply not
            // in a state where it can be honoured. Each carries `setup_path` so
            // the client renders an onboarding state pointing at the *specific*
            // missing prerequisite rather than a generic error.
            TelemetryWriteModeError::FidelityTooLow { project_id, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Telemetry Fidelity Too Low For Cloud-Primary Writes")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("setup_path", format!("/projects/{project_id}/settings"))
                    .value("blocked_by", "fidelity")
                    .build()
            }
            TelemetryWriteModeError::NotLinked { setup_path, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Not Linked To Temps Cloud")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("setup_path", setup_path)
                    .value("blocked_by", "link")
                    .build()
            }
            TelemetryWriteModeError::TelemetryExportDisabled { setup_path, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Cloud Telemetry Export Is Switched Off")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("setup_path", setup_path)
                    .value("blocked_by", "telemetry_switch")
                    .build()
            }
            TelemetryWriteModeError::CredentialRejected { setup_path, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Temps Cloud Rejected This Instance's Credential")
                    .detail(error.to_string())
                    .value("configured", false)
                    .value("setup_path", setup_path)
                    .value("blocked_by", "credential")
                    .build()
            }

            TelemetryWriteModeError::FidelityDowngradeBlockedByWriteMode { project_id, .. } => {
                ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                    .title("Cannot Lower Fidelity While Writes Are Cloud-Primary")
                    .detail(error.to_string())
                    .value("blocked_by", "write_mode")
                    .value("setup_path", format!("/projects/{project_id}/settings"))
                    .build()
            }

            // Same shape and status as the write-mode block above, because it is
            // the same refusal for the same reason at a different moment: spans
            // captured at `queryable` fidelity are still in flight, and the
            // downgrade would not stop them. `queued_spans` is carried as a
            // value so the client can render "waiting for N spans to drain"
            // rather than a dead end.
            TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
                queued_spans, ..
            } => ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
                .title("Cannot Lower Fidelity While Captured Spans Are Still Queued")
                .detail(error.to_string())
                .value("blocked_by", "queued_spans")
                .value("queued_spans", queued_spans)
                .value("setup_path", CLOUD_SETUP_PATH)
                .build(),

            TelemetryWriteModeError::Read { .. }
            | TelemetryWriteModeError::Write { .. }
            | TelemetryWriteModeError::Ledger { .. } => {
                problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Telemetry Write Mode Unavailable")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ── DTOs ───────────────────────────────────────────────────────────────────

/// A gap window as the client renders it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TelemetryGapWindowResponse {
    pub project_id: i32,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub ended_at: chrono::DateTime<chrono::Utc>,
    pub dropped_spans: i64,
    pub dropped_bytes: i64,
    pub reason: TelemetryWriteIntervalReason,
    /// The sentence shown to the operator. Sent by the server so the client
    /// never has to map an enum to prose and never renders a bare enum name.
    pub message: String,
}

impl From<temps_entities::telemetry_gap_windows::Model> for TelemetryGapWindowResponse {
    fn from(row: temps_entities::telemetry_gap_windows::Model) -> Self {
        Self {
            project_id: row.project_id,
            started_at: row.started_at,
            ended_at: row.ended_at,
            dropped_spans: row.dropped_spans,
            dropped_bytes: row.dropped_bytes,
            reason: row.reason,
            message: row.reason.message().to_string(),
        }
    }
}

/// One entry of the write-mode ledger.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TelemetryWriteIntervalResponse {
    pub mode: CloudTelemetryWriteMode,
    #[schema(value_type = String, format = DateTime)]
    pub effective_from: chrono::DateTime<chrono::Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
    pub reason: TelemetryWriteIntervalReason,
    pub message: String,
}

impl From<temps_entities::project_telemetry_write_intervals::Model>
    for TelemetryWriteIntervalResponse
{
    fn from(row: temps_entities::project_telemetry_write_intervals::Model) -> Self {
        Self {
            mode: row.mode,
            effective_from: row.effective_from,
            effective_to: row.effective_to,
            reason: row.reason,
            message: row.reason.message().to_string(),
        }
    }
}

/// A project's Cloud telemetry configuration, in every state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectCloudTelemetryResponse {
    pub project_id: i32,
    pub fidelity: CloudTelemetryFidelity,
    pub attribute_allowlist: Vec<String>,
    /// The operator's declared intent.
    pub write_mode: CloudTelemetryWriteMode,
    /// Where this project's spans are going right now, which differs from
    /// `write_mode` during a quota or credential fallback.
    pub effective_write_mode: CloudTelemetryWriteMode,
    /// Why they differ. Absent when intent and reality agree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_reason: Option<TelemetryWriteIntervalReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_reason_message: Option<String>,
    /// Whether `write_mode = cloud` could be set right now.
    ///
    /// `false` is the normal state on an unlinked instance and must render as
    /// onboarding, not as an error.
    pub cloud_write_mode_available: bool,
    /// The single most relevant missing prerequisite, when
    /// `cloud_write_mode_available` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
    /// Spans queued for Cloud for this project and not yet acknowledged.
    pub queued_spans: i64,
    /// Spans this instance accepted for this project and gave up on delivering.
    ///
    /// The instance-wide card only shows a count; this route is already scoped
    /// to one project, so it can say *why* without exposing another tenant's
    /// failures.
    pub dead_lettered_spans: i64,
    /// The reason the most recent give-up gave, if there is one.
    ///
    /// Delivery metadata only — this instance's own bounded error string. It is
    /// never the span payload, and this field must not become a place one
    /// appears.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_dead_letter_error: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_dead_letter_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Gap windows in the last 30 days.
    pub gap_windows: Vec<TelemetryGapWindowResponse>,
    /// The write-mode ledger, newest first.
    pub intervals: Vec<TelemetryWriteIntervalResponse>,
}

/// Instance-wide aggregate for the Cloud settings page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CloudTelemetryWriteStatusResponse {
    /// Whether Cloud-primary writes can be used at all on this instance.
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,

    pub cloud_primary_projects: u64,
    pub local_mode_projects: u64,

    /// The derived decommission signal. `true` means the operator gets **zero**
    /// resource win from removing their local span store, whatever the write
    /// modes say.
    pub local_span_store_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_span_store_reason: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_history_until: Option<chrono::DateTime<chrono::Utc>>,

    /// Spans queued for Cloud across all projects.
    pub queue_depth: i64,
    pub queue_bytes: i64,
    /// The operator-set ceiling, so the depth above is readable against
    /// something rather than being a number with no scale.
    pub queue_max_bytes: u64,
    /// Age of the oldest unshipped span, in seconds. `None` when the queue is
    /// empty — different from zero, and it must not render as zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_unshipped_age_secs: Option<i64>,
    /// Rows that exhausted their retries. Never swept automatically; an
    /// operator has to see them.
    pub dead_lettered_rows: i64,

    /// Set while Cloud-primary writes are falling back to local storage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_suspension: Option<String>,

    pub gap_windows: Vec<TelemetryGapWindowResponse>,

    /// Whether the console should offer decommission guidance. True only when
    /// `local_span_store_required` is false — the ADR is explicit that the
    /// advice must not appear before then.
    pub can_decommission_local_span_store: bool,
}

/// Body for changing a project's Cloud telemetry settings.
///
/// Both fields are optional and independent: an operator raising fidelity and
/// one flipping the write mode are two different acts, and forcing a client to
/// send both would make each one able to clobber the other.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProjectCloudTelemetryRequest {
    #[serde(default)]
    pub fidelity: Option<CloudTelemetryFidelity>,
    #[serde(default)]
    pub attribute_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub write_mode: Option<CloudTelemetryWriteMode>,
}

// ── Handlers ───────────────────────────────────────────────────────────────

/// A project's Cloud telemetry write mode and fidelity.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/cloud-telemetry/projects/{project_id}",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project Cloud telemetry settings", body = ProjectCloudTelemetryResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_cloud_telemetry(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let response = build_project_response(&state, project_id).await?;
    Ok(Json(response))
}

/// Change a project's Cloud telemetry write mode and/or fidelity.
#[utoipa::path(
    tag = "OTel",
    patch,
    path = "/otel/cloud-telemetry/projects/{project_id}",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = UpdateProjectCloudTelemetryRequest,
    responses(
        (status = 200, description = "Updated settings", body = ProjectCloudTelemetryResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 409, description = "A prerequisite is missing", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_project_cloud_telemetry(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
    axum::Extension(metadata): axum::Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<UpdateProjectCloudTelemetryRequest>,
) -> Result<impl IntoResponse, Problem> {
    // Deliberately `OtelWrite`, not a read permission: this decides whether a
    // project's spans are stored on this machine at all.
    permission_guard!(auth, OtelWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let before = state.telemetry_write_modes.settings(project_id).await?;

    // Fidelity first. Raising it is what makes a Cloud-primary write mode
    // reachable in the same request; lowering it while the mode is already
    // `cloud` is refused by the service, which is exactly the ordering that
    // keeps the forbidden pair unreachable.
    if request.fidelity.is_some() || request.attribute_allowlist.is_some() {
        state
            .telemetry_write_modes
            .set_fidelity(
                project_id,
                request.fidelity.unwrap_or(before.fidelity),
                request.attribute_allowlist.clone(),
            )
            .await?;
    }

    if let Some(write_mode) = request.write_mode {
        state
            .telemetry_write_modes
            .set_write_mode(project_id, write_mode, link_snapshot(&state))
            .await?;
    }

    let after = state.telemetry_write_modes.settings(project_id).await?;

    let audit = crate::handlers::audit::CloudTelemetryWriteModeChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        previous_write_mode: before.write_mode.to_string(),
        write_mode: after.write_mode.to_string(),
        previous_fidelity: before.fidelity.to_string(),
        fidelity: after.fidelity.to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    let response = build_project_response(&state, project_id).await?;
    Ok(Json(response))
}

/// Instance-wide Cloud telemetry write status.
#[utoipa::path(
    tag = "OTel",
    get,
    path = "/otel/cloud-telemetry/status",
    responses(
        (status = 200, description = "Instance Cloud telemetry write status", body = CloudTelemetryWriteStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cloud_telemetry_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<OtelAppState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, OtelRead);

    let link = link_snapshot(&state);
    let (configured, reason, setup_path) = instance_capability(&link);

    let requirement = state
        .telemetry_write_modes
        .local_span_store_requirement(local_retention_days())
        .await?;

    let (queue_depth, queue_bytes, oldest, dead_lettered, queue_max_bytes) =
        match state.otel_service.span_outbox() {
            Some(outbox) => match outbox.stats().await {
                Ok(stats) => (
                    stats.pending_rows,
                    stats.pending_bytes,
                    stats.oldest_pending_age_secs,
                    stats.dead_letter_rows,
                    outbox.max_bytes(),
                ),
                Err(error) => {
                    // The queue's own state is unavailable. Reporting zeros
                    // would say "nothing is queued", which is a claim this
                    // instance cannot make right now.
                    error!(%error, "Could not read the Cloud telemetry outbox statistics");
                    return Err(
                        problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                            .with_title("Telemetry Queue Status Unavailable")
                            .with_detail(format!(
                        "Could not read the Temps Cloud telemetry outbox: {error}. Queue depth \
                         and gap windows cannot be reported until this succeeds."
                    )),
                    );
                }
            },
            None => (0, 0, None, 0, 0),
        };

    // The aggregate counters above are instance-level and carry no project
    // identity, so they stay unfiltered. Each gap window does not: it names a
    // `project_id` and states when and how much of that project's telemetry was
    // lost. On an instance running per-project access control, an `OtelRead`
    // holder with no access to a project — `Role::Reader` is enough — would
    // otherwise learn that project's id, its telemetry volume and its outage
    // windows from this list. Same filter, same admin bypass, as every other
    // cross-project list in this codebase.
    let hidden = hidden_project_ids(&auth, &state).await?;
    let gap_windows = visible_recent_gap_windows(
        state
            .telemetry_write_modes
            .recent_gap_windows(MAX_ITEMS)
            .await?,
        &hidden,
        chrono::Utc::now(),
    );

    let suspension = state.telemetry_write_modes.suspension();
    let write_suspension = suspension.is_suspended().then(|| {
        state
            .telemetry_write_modes
            .suspension_detail()
            .unwrap_or_else(|| suspension.interval_reason().message().to_string())
    });

    Ok(Json(CloudTelemetryWriteStatusResponse {
        configured,
        reason,
        setup_path,
        cloud_primary_projects: requirement.cloud_primary_projects,
        local_mode_projects: requirement.local_mode_projects,
        can_decommission_local_span_store: !requirement.required,
        local_span_store_required: requirement.required,
        local_span_store_reason: requirement.reason.clone(),
        local_history_until: requirement.local_history_until,
        queue_depth,
        queue_bytes,
        queue_max_bytes,
        oldest_unshipped_age_secs: oldest,
        dead_lettered_rows: dead_lettered,
        write_suspension,
        gap_windows,
    }))
}

// ── Shared assembly ────────────────────────────────────────────────────────

/// Projects this caller must not see named, as a set.
///
/// Empty for an admin or platform admin, and empty on an instance with no
/// project-access plugin — the same two bypasses every other call site of
/// `hidden_project_ids` uses, so this route cannot become the one place where
/// per-project visibility works differently.
///
/// A failure here is a 500 rather than an unfiltered list: "we could not check
/// what you may see" must never resolve to "show everything".
async fn hidden_project_ids(
    auth: &temps_auth::AuthContext,
    state: &OtelAppState,
) -> Result<std::collections::HashSet<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&temps_auth::Role::PlatformAdmin) {
        return Ok(std::collections::HashSet::new());
    }
    let (Some(checker), Some(user_id)) =
        (state.project_access_checker.as_ref(), auth.user_id_opt())
    else {
        return Ok(std::collections::HashSet::new());
    };

    let hidden = checker.hidden_project_ids(user_id).await.map_err(|error| {
        error!(
            user_id,
            %error,
            "Project visibility check failed while listing Cloud telemetry gap windows"
        );
        problemdetails::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Project Access Check Failed")
            .with_detail(
                "Could not verify which projects you may see, so the per-project Cloud telemetry \
                 gap windows cannot be listed.",
            )
    })?;

    Ok(hidden.unwrap_or_default().into_iter().collect())
}

/// Narrow the instance-wide gap-window list to the recent ones this caller may
/// see.
///
/// Pure, because it is the boundary that keeps one tenant's project ids,
/// telemetry volume and outage windows out of another's status page, and that
/// is worth being able to assert without standing up an HTTP stack.
fn visible_recent_gap_windows(
    rows: Vec<temps_entities::telemetry_gap_windows::Model>,
    hidden: &std::collections::HashSet<i32>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<TelemetryGapWindowResponse> {
    let cutoff = now - chrono::Duration::days(HISTORY_DAYS);
    rows.into_iter()
        .filter(|row| row.ended_at >= cutoff)
        .filter(|row| !hidden.contains(&row.project_id))
        .map(TelemetryGapWindowResponse::from)
        .collect()
}

pub(crate) fn link_snapshot(state: &OtelAppState) -> CloudLinkSnapshot {
    match &state.cloud_link {
        Some(link) => CloudLinkSnapshot {
            linked: link.is_linked(),
            telemetry_enabled: link.telemetry_enabled(),
            credential_rejected: matches!(
                link.status(),
                temps_cloud_client::LinkStatus::CredentialRejected { .. }
            ),
        },
        // No Cloud integration compiled in at all. "Not linked" is the truthful
        // answer and produces the onboarding state, which is what a user of a
        // build without Cloud should see.
        None => CloudLinkSnapshot::default(),
    }
}

/// Local OTel span retention, used to decide whether pre-cutover history still
/// keeps the local store required.
///
/// Uses the same default as `ObservabilityRetentionSettings`. The handler does
/// not read `ConfigService` here because `temps-otel`'s app state does not
/// carry one; the number only affects a advisory message, and being wrong in
/// the conservative direction (claiming history is still readable) keeps the
/// decommission guidance from appearing too early.
pub(crate) fn local_retention_days() -> u32 {
    temps_core::ObservabilityRetentionSettings::default().otel_spans_days
}

/// The instance-level answer to "can any project be Cloud-primary right now".
pub(crate) fn instance_capability(
    link: &CloudLinkSnapshot,
) -> (bool, Option<String>, Option<String>) {
    if !link.linked {
        return (
            false,
            Some(
                "This instance is not linked to Temps Cloud, so no project's spans can be written \
                 there. Cloud-primary telemetry writes stop a project's spans being stored on \
                 this instance at all, which is how a local span store — ClickHouse, or the \
                 `otel_spans` hypertable — becomes unnecessary."
                    .to_string(),
            ),
            Some(CLOUD_SETUP_PATH.to_string()),
        );
    }
    if link.credential_rejected {
        return (
            false,
            Some(
                "Temps Cloud rejected this instance's credential, so nothing can be shipped. \
                 Re-enroll the instance to use Cloud-primary telemetry writes."
                    .to_string(),
            ),
            Some(CLOUD_SETUP_PATH.to_string()),
        );
    }
    if !link.telemetry_enabled {
        return (
            false,
            Some(
                "Temps Cloud telemetry export is switched off for this instance, so no span would \
                 leave it. Turn telemetry export on to use Cloud-primary telemetry writes."
                    .to_string(),
            ),
            Some(CLOUD_SETUP_PATH.to_string()),
        );
    }
    (true, None, None)
}

/// The per-project answer, including the reason a project specifically cannot
/// be Cloud-primary yet.
fn project_capability(
    link: &CloudLinkSnapshot,
    settings: &ProjectTelemetryWriteSettings,
) -> (bool, Option<String>, Option<String>) {
    // Fidelity is checked first because it is the prerequisite the operator can
    // fix on the page they are already looking at.
    if !settings.fidelity.is_queryable() {
        return (
            false,
            Some(format!(
                "This project's Cloud telemetry fidelity is `{}`. Cloud-primary writes require \
                 `queryable`, because `metered` spans are pseudonymised placeholders that cannot \
                 be read back — the project's traces would exist nowhere.",
                settings.fidelity
            )),
            Some(format!("/projects/{}/settings", settings.project_id)),
        );
    }
    instance_capability(link)
}

async fn build_project_response(
    state: &OtelAppState,
    project_id: i32,
) -> Result<ProjectCloudTelemetryResponse, Problem> {
    let settings = state.telemetry_write_modes.settings(project_id).await?;
    let link = link_snapshot(state);
    let (available, reason, setup_path) = project_capability(&link, &settings);

    let now = chrono::Utc::now();
    let gap_windows = state
        .telemetry_write_modes
        .gap_windows(
            project_id,
            now - chrono::Duration::days(HISTORY_DAYS),
            now,
            MAX_ITEMS,
        )
        .await?
        .into_iter()
        .map(TelemetryGapWindowResponse::from)
        .collect();

    let intervals = state
        .telemetry_write_modes
        .intervals(project_id, MAX_ITEMS)
        .await?
        .into_iter()
        .map(TelemetryWriteIntervalResponse::from)
        .collect();

    let (queued_spans, dead_letters) = match state.otel_service.span_outbox() {
        Some(outbox) => (
            outbox
                .pending_rows_for_project(project_id)
                .await
                .unwrap_or(0),
            outbox
                .dead_letter_summary_for_project(project_id)
                .await
                .unwrap_or_default(),
        ),
        None => (0, temps_cloud_client::DeadLetterSummary::default()),
    };

    Ok(ProjectCloudTelemetryResponse {
        project_id,
        fidelity: settings.fidelity,
        attribute_allowlist: settings.attribute_allowlist.clone(),
        write_mode: settings.write_mode,
        effective_write_mode: settings.effective_mode,
        effective_reason: settings.effective_reason,
        effective_reason_message: settings
            .effective_reason
            .map(|reason| reason.message().to_string()),
        cloud_write_mode_available: available,
        reason,
        setup_path,
        queued_spans,
        dead_lettered_spans: dead_letters.rows,
        last_dead_letter_error: dead_letters.last_error,
        last_dead_letter_at: dead_letters.last_settled_at,
        gap_windows,
        intervals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn settings(
        fidelity: CloudTelemetryFidelity,
        write_mode: CloudTelemetryWriteMode,
    ) -> ProjectTelemetryWriteSettings {
        ProjectTelemetryWriteSettings {
            project_id: 7,
            fidelity,
            write_mode,
            attribute_allowlist: Vec::new(),
            effective_mode: write_mode,
            effective_reason: None,
        }
    }

    fn healthy_link() -> CloudLinkSnapshot {
        CloudLinkSnapshot {
            linked: true,
            telemetry_enabled: true,
            credential_rejected: false,
        }
    }

    // ── Feature Discoverability: the control never disappears ────────────

    #[test]
    fn an_unlinked_instance_gets_an_onboarding_reason_and_a_setup_path() {
        // Not an error. The control renders, says what it would do, says what
        // is missing, and links to the fix.
        let (available, reason, setup_path) = project_capability(
            &CloudLinkSnapshot::default(),
            &settings(
                CloudTelemetryFidelity::Queryable,
                CloudTelemetryWriteMode::Local,
            ),
        );

        assert!(!available);
        let reason = reason.expect("an unavailable control must say why");
        assert!(reason.contains("not linked"), "{reason}");
        // An onboarding state has to describe the capability, not just report
        // its absence: "not linked" alone tells an operator nothing about why
        // they would want to link.
        assert!(
            reason.contains("spans being stored on this instance"),
            "must say what the feature would do: {reason}"
        );
        assert!(
            reason.contains("local span store"),
            "must name the thing the operator would save: {reason}"
        );
        assert_eq!(setup_path.as_deref(), Some(CLOUD_SETUP_PATH));
    }

    #[test]
    fn a_metered_project_is_told_about_fidelity_not_about_the_link() {
        // The prerequisite the operator can fix on the page they are on comes
        // first; sending them to /settings/cloud for a fidelity problem wastes
        // their time on the one thing that is not wrong.
        let (available, reason, setup_path) = project_capability(
            &healthy_link(),
            &settings(
                CloudTelemetryFidelity::Metered,
                CloudTelemetryWriteMode::Local,
            ),
        );

        assert!(!available);
        let reason = reason.expect("must say why");
        assert!(reason.contains("queryable"), "{reason}");
        assert!(!reason.contains("not linked"), "{reason}");
        assert_eq!(setup_path.as_deref(), Some("/projects/7/settings"));
    }

    #[test]
    fn a_switched_off_telemetry_export_is_distinguishable_from_an_unlinked_instance() {
        let switched_off = CloudLinkSnapshot {
            linked: true,
            telemetry_enabled: false,
            credential_rejected: false,
        };
        let (available, reason, _) = instance_capability(&switched_off);
        assert!(!available);
        let reason = reason.expect("must say why");
        assert!(reason.contains("switched off"), "{reason}");

        let (_, unlinked_reason, _) = instance_capability(&CloudLinkSnapshot::default());
        assert_ne!(unlinked_reason, Some(reason));
    }

    #[test]
    fn a_rejected_credential_is_reported_before_the_telemetry_switch() {
        // A rejected credential makes the telemetry switch irrelevant, and
        // telling the operator to check a switch that is already on would send
        // them looking in the wrong place.
        let rejected = CloudLinkSnapshot {
            linked: true,
            telemetry_enabled: true,
            credential_rejected: true,
        };
        let (available, reason, _) = instance_capability(&rejected);
        assert!(!available);
        assert!(reason.expect("must say why").contains("Re-enroll"));
    }

    #[test]
    fn a_healthy_link_and_queryable_fidelity_is_available_with_no_reason() {
        let (available, reason, setup_path) = project_capability(
            &healthy_link(),
            &settings(
                CloudTelemetryFidelity::Queryable,
                CloudTelemetryWriteMode::Local,
            ),
        );
        assert!(available);
        assert!(reason.is_none(), "an available control must not nag");
        assert!(setup_path.is_none());
    }

    // ── Error mapping ────────────────────────────────────────────────────

    #[test]
    fn each_gate_refusal_maps_to_a_conflict_carrying_its_own_blocked_by() {
        // 409, not 400 or 500: the request is well-formed and the feature
        // exists — the instance is not in a state where it can be honoured.
        for error in [
            TelemetryWriteModeError::FidelityTooLow {
                project_id: 7,
                fidelity: CloudTelemetryFidelity::Metered,
            },
            TelemetryWriteModeError::NotLinked {
                project_id: 7,
                setup_path: CLOUD_SETUP_PATH,
            },
            TelemetryWriteModeError::TelemetryExportDisabled {
                project_id: 7,
                setup_path: CLOUD_SETUP_PATH,
            },
            TelemetryWriteModeError::CredentialRejected {
                project_id: 7,
                setup_path: CLOUD_SETUP_PATH,
            },
        ] {
            let problem = Problem::from(error);
            assert_eq!(
                problem.status_code,
                axum::http::StatusCode::CONFLICT,
                "a missing prerequisite is a conflict"
            );
        }
    }

    #[test]
    fn a_missing_project_is_a_404_not_a_conflict() {
        let problem = Problem::from(TelemetryWriteModeError::ProjectNotFound { project_id: 404 });
        assert_eq!(problem.status_code, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_ledger_failure_is_a_500_and_never_pretends_the_write_succeeded() {
        let problem = Problem::from(TelemetryWriteModeError::Ledger {
            source: sea_orm::DbErr::Custom("connection reset".into()),
        });
        assert_eq!(
            problem.status_code,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn the_local_retention_default_matches_the_settings_default() {
        assert_eq!(
            local_retention_days(),
            temps_core::ObservabilityRetentionSettings::default().otel_spans_days
        );
    }

    // ── The instance status page is not a cross-tenant directory ─────────

    fn gap(project_id: i32, ended_days_ago: i64) -> temps_entities::telemetry_gap_windows::Model {
        let ended_at = chrono::Utc::now() - chrono::Duration::days(ended_days_ago);
        temps_entities::telemetry_gap_windows::Model {
            id: project_id as i64,
            project_id,
            started_at: ended_at - chrono::Duration::minutes(30),
            ended_at,
            dropped_spans: 1_000,
            dropped_bytes: 500_000,
            reason: TelemetryWriteIntervalReason::QueueOverflowSpill,
        }
    }

    #[test]
    fn a_gap_window_for_a_project_the_caller_cannot_see_is_not_listed() {
        // Every field of a gap window is a fact about one project: its id, how
        // much of its telemetry was lost, and exactly when it was having an
        // outage. On an instance with per-project access control, the lowest
        // role holding `OtelRead` must not learn any of that about a project it
        // has no access to.
        let hidden: std::collections::HashSet<i32> = [42].into_iter().collect();
        let visible = visible_recent_gap_windows(vec![gap(7, 1), gap(42, 1)], &hidden, Utc::now());

        assert_eq!(visible.len(), 1, "the hidden project's row must be dropped");
        assert_eq!(visible[0].project_id, 7);
        assert!(
            !visible.iter().any(|row| row.project_id == 42),
            "a hidden project's id must not appear anywhere in the response"
        );
    }

    #[test]
    fn an_admin_with_nothing_hidden_still_sees_every_project() {
        // The filter must be a *narrowing*, never a change in what the operator
        // running the instance can read about their own instance.
        let visible = visible_recent_gap_windows(
            vec![gap(7, 1), gap(42, 1)],
            &std::collections::HashSet::new(),
            Utc::now(),
        );
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn gap_windows_older_than_the_history_horizon_are_still_dropped() {
        // The scoping must not have replaced the age bound it was added beside.
        let visible = visible_recent_gap_windows(
            vec![gap(7, HISTORY_DAYS + 5), gap(8, 1)],
            &std::collections::HashSet::new(),
            Utc::now(),
        );
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].project_id, 8);
    }

    #[test]
    fn a_downgrade_blocked_by_queued_spans_is_a_conflict_that_names_the_depth() {
        // 409, like every other "the instance is not in a state where this can
        // be honoured" refusal — and it has to say how many spans are still in
        // flight, or the operator has no way to know whether to wait a second or
        // look for a stuck queue.
        let error = TelemetryWriteModeError::FidelityDowngradeBlockedByQueuedSpans {
            project_id: 7,
            requested: CloudTelemetryFidelity::Metered,
            queued_spans: 1_234,
            spill_blocked_reason: "the local span store did not accept them",
        };
        let message = error.to_string();
        assert!(message.contains("1234"), "{message}");
        assert!(
            message.contains("queued for delivery to Temps Cloud"),
            "the operator must be told these spans are still going to leave: {message}"
        );

        let problem = Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
    }
}
