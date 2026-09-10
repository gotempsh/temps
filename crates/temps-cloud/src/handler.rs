// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::ErrorBuilder, problemdetails::Problem, AuditContext, AuditLogger,
    AuditOperation, CloudTelemetryActivationTrigger, RequestMetadata, TelemetryActivationOutcome,
    TelemetryActivationSkipped,
};
use utoipa::{OpenApi, ToSchema};

use crate::{
    CloudAiCapability, CloudCapability, CloudService, CloudServiceError, CloudStatus,
    ManagedBackupOutcome, ManagedBackupSetup, ManagedBackupSetupAction, ManagedBackupSetupStatus,
};
use temps_cloud_client::CloudFeatureSwitches;

#[derive(Clone)]
pub struct CloudState {
    service: Arc<CloudService>,
    audit: Arc<dyn AuditLogger>,
    /// ADR-042 P3: starts the Cloud telemetry activation a successful
    /// enrollment just paid for.
    ///
    /// `None` on a build with no OTel plugin, or an instance with no local span
    /// source — in which case enrollment behaves exactly as it did before this
    /// existed, which is the property the ADR's "enroll-path coupling" risk
    /// asks for.
    activation: Option<Arc<dyn CloudTelemetryActivationTrigger>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnrollCloudRequest {
    #[schema(min_length = 1, example = "ABCD-EFGH")]
    pub enrollment_code: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CloudFeatureSwitchesRequest {
    pub telemetry_enabled: bool,
    pub backups_enabled: bool,
    pub notifications_enabled: bool,
}

#[derive(Debug, Serialize)]
struct CloudLinkAudit {
    context: AuditContext,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_features: Option<CloudFeatureAuditValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_features: Option<CloudFeatureAuditValues>,
}

#[derive(Debug, Serialize)]
struct CloudFeatureAuditValues {
    telemetry_enabled: bool,
    backups_enabled: bool,
    notifications_enabled: bool,
}

impl From<CloudFeatureSwitches> for CloudFeatureAuditValues {
    fn from(value: CloudFeatureSwitches) -> Self {
        Self {
            telemetry_enabled: value.telemetry,
            backups_enabled: value.backups,
            notifications_enabled: value.notifications,
        }
    }
}

/// `cloud.telemetry_activation.started` — the purchase-triggered activation
/// queued by a successful enrollment (ADR-042 P3).
///
/// A separate event from `CLOUD_LINK_CONNECTED` because it records a *spend*,
/// not a link: what this instance believed the activation would cost, over which
/// projects, before anything was sent. The actuals land on the job row, and a
/// customer disputing an invoice needs both.
///
/// It is written from a background task, after the enroll response has already
/// gone out, because that is where the job id first exists. The alternative —
/// holding the response until every project has been estimated — would make an
/// enrollment on a large instance appear to hang.
#[derive(Debug, Serialize)]
struct CloudTelemetryActivationStartedAudit {
    context: AuditContext,
    /// The job id, so this row and `GET /bulk-jobs/{batch_id}` agree.
    batch_id: String,
    /// Always `purchase` here. The operator path writes its own event.
    trigger: &'static str,
    project_ids: Vec<i32>,
    project_count: usize,
    estimated_spans: i64,
    estimated_bytes: i64,
    window_from: String,
    window_to: String,
}

impl AuditOperation for CloudTelemetryActivationStartedAudit {
    fn operation_type(&self) -> String {
        "cloud.telemetry_activation.started".to_string()
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
        serde_json::to_string(self).map_err(Into::into)
    }
}

impl AuditOperation for CloudLinkAudit {
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
        serde_json::to_string(self).map_err(Into::into)
    }
}

fn problem(error: CloudServiceError) -> Problem {
    let status = match &error {
        CloudServiceError::Client(temps_cloud_client::CloudError::EnrollmentRefused { .. }) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        CloudServiceError::Client(
            temps_cloud_client::CloudError::Unreachable { .. }
            | temps_cloud_client::CloudError::BackupUploadIdleTimeout { .. },
        ) => StatusCode::SERVICE_UNAVAILABLE,
        CloudServiceError::Client(temps_cloud_client::CloudError::CredentialRejected) => {
            StatusCode::UNAUTHORIZED
        }
        CloudServiceError::Client(
            temps_cloud_client::CloudError::NotEnrolled
            | temps_cloud_client::CloudError::LinkStateUnreadable { .. }
            | temps_cloud_client::CloudError::ConfigurationBlocked { .. },
        ) => StatusCode::CONFLICT,
        CloudServiceError::Client(temps_cloud_client::CloudError::FeatureDisabled { .. }) => {
            StatusCode::CONFLICT
        }
        CloudServiceError::Client(
            temps_cloud_client::CloudError::Rejected { .. }
            | temps_cloud_client::CloudError::InvalidAcknowledgement { .. }
            | temps_cloud_client::CloudError::InvalidBackupTarget { .. },
        ) => StatusCode::BAD_GATEWAY,
        CloudServiceError::Configuration(_)
        | CloudServiceError::InvalidBackend { .. }
        | CloudServiceError::State(_)
        | CloudServiceError::Database(_)
        | CloudServiceError::ManagedBackupCredential(_)
        | CloudServiceError::Client(
            temps_cloud_client::CloudError::InvalidBackendUrl { .. }
            | temps_cloud_client::CloudError::ClientConfiguration { .. },
        ) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let detail = if status.is_server_error() {
        tracing::error!(%error, http_status = status.as_u16(), "managed control-plane request failed");
        "Managed control-plane operation could not be completed. Check the server logs for details."
            .to_string()
    } else {
        error.to_string()
    };
    ErrorBuilder::new(status)
        .type_("https://temps.sh/probs/cloud-link")
        .title("Managed control plane error")
        .detail(detail)
        .build()
}

#[utoipa::path(get, path = "/cloud/capability", tag = "Cloud", responses((status = 200, body = CloudCapability)), security(("bearer_auth" = [])))]
async fn get_cloud_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
) -> Result<Json<CloudCapability>, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.capability().await))
}

#[utoipa::path(get, path = "/cloud/status", tag = "Cloud", responses((status = 200, body = CloudStatus)), security(("bearer_auth" = [])))]
async fn get_cloud_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
) -> Result<Json<CloudStatus>, Problem> {
    permission_guard!(auth, SettingsRead);
    state.service.status().await.map(Json).map_err(problem)
}

#[utoipa::path(get, path = "/cloud/ai/capability", tag = "Cloud", responses((status = 200, body = CloudAiCapability)), security(("bearer_auth" = [])))]
async fn get_cloud_ai_capability(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
) -> Result<Json<CloudAiCapability>, Problem> {
    permission_guard!(auth, SettingsRead);
    state
        .service
        .ai_capability()
        .await
        .map(Json)
        .map_err(problem)
}

#[utoipa::path(patch, path = "/cloud/features", tag = "Cloud", request_body = CloudFeatureSwitchesRequest, responses((status = 200, body = CloudStatus)), security(("bearer_auth" = [])))]
async fn update_cloud_features(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CloudFeatureSwitchesRequest>,
) -> Result<Json<CloudStatus>, Problem> {
    permission_guard!(auth, SettingsWrite);
    if request.backups_enabled {
        permission_guard!(auth, BackupsWrite);
    }
    if request.notifications_enabled {
        permission_guard!(auth, NotificationProvidersWrite);
        permission_guard!(auth, NotificationProvidersCreate);
    }
    let previous = state.service.feature_switches();
    let switches = CloudFeatureSwitches {
        telemetry: request.telemetry_enabled,
        backups: request.backups_enabled,
        notifications: request.notifications_enabled,
    };
    let result = state
        .service
        .update_feature_switches(switches)
        .await
        .map_err(problem)?;
    audit(
        &state,
        &auth,
        &metadata,
        "CLOUD_FEATURES_UPDATED",
        Some(previous),
        Some(switches),
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(post, path = "/cloud/enroll", tag = "Cloud", request_body = EnrollCloudRequest, responses((status = 200, body = CloudStatus)), security(("bearer_auth" = [])))]
async fn enroll_cloud(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<EnrollCloudRequest>,
) -> Result<Json<CloudStatus>, Problem> {
    permission_guard!(auth, SettingsWrite);
    if request.enrollment_code.trim().is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .detail("Enrollment code cannot be empty")
            .build());
    }
    let (result, backup_outcome, enrollment) = state
        .service
        .enroll(&request.enrollment_code)
        .await
        .map_err(problem)?;
    audit(&state, &auth, &metadata, "CLOUD_LINK_CONNECTED", None, None).await;
    match &backup_outcome {
        ManagedBackupOutcome::Provisioned => {
            audit(
                &state,
                &auth,
                &metadata,
                "cloud.backup_credential.issued",
                None,
                None,
            )
            .await;
        }
        // A distinct, loud action name: this should never happen under a
        // correct backend tenant->bucket contract (see the doc comment on
        // this variant). Full detail (both bucket names) is already in the
        // server log via the `tracing::error!` in `provision_managed_backup_source`;
        // the audit trail just needs to make clear this run differs from a
        // routine rotation, since it means backups may have been orphaned.
        ManagedBackupOutcome::ProvisionedBucketChanged { .. } => {
            audit(
                &state,
                &auth,
                &metadata,
                "cloud.backup_credential.bucket_changed",
                None,
                None,
            )
            .await;
        }
        ManagedBackupOutcome::NotConfigured { .. } | ManagedBackupOutcome::Unavailable(_) => {}
    }

    // ADR-042 P3: the telemetry activation the customer just paid for, started
    // here for the same reason the managed backup source above is provisioned
    // here — enrollment already has automatic side effects, and this is one of
    // them (ADR-042, Finding 4).
    //
    // The handle is dropped, not awaited: dropping it detaches the task rather
    // than cancelling it, which is exactly the fire-and-forget semantics the ADR
    // requires. It is returned at all so a test can join it deterministically.
    let _activation = if may_start_activation(&auth, enrollment) {
        start_telemetry_activation(
            state.activation.clone(),
            state.audit.clone(),
            AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
        )
    } else {
        None
    };

    Ok(Json(result))
}

/// Whether *this* enrollment, by *this* principal, may auto-start a spend.
///
/// Two independent gates, and both are on the side effect rather than on
/// `POST /cloud/enroll` itself. Enrollment predates the activation and has
/// callers with nothing to do with telemetry — tightening the endpoint's
/// contract would break them to fix a problem that lives entirely in the thing
/// enrollment now triggers.
///
/// # 1. Only an enrollment that establishes a link
///
/// `CloudLink::enroll` deliberately overwrites the credential on an
/// already-linked instance, because that is how an operator recovers from
/// `CredentialRejected`. Firing an activation on every successful enroll
/// therefore means a routine re-authentication silently re-activates every
/// project the operator left in `local` mode after the first activation —
/// undoing a deliberate decision, at the operator's expense, with nothing on
/// screen saying it happened. A re-enrollment to the same tenant is not a
/// purchase.
///
/// # 2. Only a principal who runs the instance
///
/// The operator path to the same engine requires `OtelWrite` **and**
/// [`is_instance_admin`](temps_auth::AuthContext::is_instance_admin), because
/// switching every project's telemetry to Cloud and shipping its history is
/// instance-wide by construction. A `SettingsWrite` holder reaching the same
/// spend through the enroll endpoint would make the confirmed path's bar
/// decorative. Enrollment itself still succeeds for them — backup provisioning,
/// feature switches and the link are unaffected — the activation simply is not
/// queued, and a qualified administrator can start it from the status card.
fn may_start_activation(
    auth: &temps_auth::AuthContext,
    enrollment: temps_cloud_client::EnrollmentKind,
) -> bool {
    if !enrollment.establishes_new_link() {
        tracing::info!(
            enrollment = enrollment.as_str(),
            "Temps Cloud enrollment re-authenticated an existing link to the same tenant; no \
             telemetry activation was queued, because the projects this instance keeps storing \
             locally were a deliberate choice made after the first activation. Activate more \
             projects from Settings -> Temps Cloud."
        );
        return false;
    }
    if !auth.is_instance_admin() {
        tracing::info!(
            enrollment = enrollment.as_str(),
            user_role = %auth.effective_role,
            "Temps Cloud enrollment succeeded but no telemetry activation was queued: starting \
             one switches every local project's spans to Temps Cloud and ships their history at \
             this instance's expense, which is restricted to an instance administrator. An \
             administrator can start it from Settings -> Temps Cloud."
        );
        return false;
    }
    true
}

/// Kick off the purchase-triggered activation without holding the response.
///
/// # Why this is spawned and not awaited
///
/// ADR-042 names the risk directly: *"a bug in job creation could fail an
/// enrollment. Job creation must be fire-and-forget with respect to enroll's
/// response."* Two things follow, and both need the spawn:
///
/// 1. **No error may escape.** Every outcome — a refused job, a database
///    failure, an audit write that fails — is logged here and goes no further.
///    By the time this runs, the enrollment has already succeeded; failing it
///    retroactively would leave a customer who has paid for Temps Cloud looking
///    at an error, with the link nevertheless established.
/// 2. **No latency may escape either.** The work estimates every local project
///    in turn — an exact `count_spans_window` plus a 1,000-span projection each.
///    Awaiting that on an instance with forty projects would make `POST
///    /cloud/enroll` appear to hang at the exact moment a customer is watching
///    it most closely.
///
/// The job's own state is not lost by being detached: it is a row in
/// `cloud_telemetry_bulk_jobs` the moment it is created, the worker picks it up
/// from there, and the activation status card renders it. If this task dies
/// before creating one, nothing was queued and the card offers the operator
/// path's button — which is the honest state, not a silent one.
fn start_telemetry_activation(
    activation: Option<Arc<dyn CloudTelemetryActivationTrigger>>,
    audit_logger: Arc<dyn AuditLogger>,
    context: AuditContext,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(activation) = activation else {
        // No OTel plugin, or no local span source: there is no history to ship
        // and nothing to switch. Debug rather than warn — this is a valid build,
        // not a misconfiguration.
        tracing::debug!(
            "no Cloud telemetry activation trigger is registered; enrollment linked the \
             instance without queueing an activation"
        );
        return None;
    };

    Some(tokio::spawn(async move {
        let outcome = match activation.start_purchase_activation().await {
            Ok(outcome) => outcome,
            Err(error) => {
                // The enrollment stands. The activation's failure surfaces on
                // the Cloud telemetry status card, which shows no running job
                // and offers the operator-path button — so the customer can
                // start it themselves rather than being stuck.
                tracing::error!(
                    %error,
                    "Temps Cloud enrollment succeeded but the telemetry activation could not be \
                     queued. Projects were not switched and no history was shipped; start the \
                     activation from Settings -> Temps Cloud."
                );
                return;
            }
        };

        let started = match outcome {
            TelemetryActivationOutcome::Started(started) => started,
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NoLocalProjects) => {
                tracing::info!(
                    "Temps Cloud enrollment queued no telemetry activation: every project on \
                     this instance already writes its spans to Temps Cloud, or there are no \
                     projects yet. New projects can be activated from Settings -> Temps Cloud."
                );
                return;
            }
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::NotConfigured {
                reason,
                setup_path,
            }) => {
                tracing::info!(
                    setup_path = setup_path.as_deref().unwrap_or("-"),
                    "Temps Cloud enrollment queued no telemetry activation: {reason}"
                );
                return;
            }
            TelemetryActivationOutcome::Skipped(TelemetryActivationSkipped::AlreadyActive {
                batch_id,
            }) => {
                tracing::info!(
                    %batch_id,
                    "Temps Cloud enrollment queued no telemetry activation: one is already \
                     running. This instance may have exactly one Cloud submission in flight."
                );
                return;
            }
        };

        tracing::info!(
            batch_id = %started.batch_id,
            projects = started.project_count(),
            estimated_spans = started.estimated_spans,
            estimated_bytes = started.estimated_bytes,
            "Temps Cloud enrollment queued a telemetry activation for every project still \
             storing spans on this instance"
        );

        let event = CloudTelemetryActivationStartedAudit {
            context,
            batch_id: started.batch_id.clone(),
            trigger: "purchase",
            project_count: started.project_count(),
            project_ids: started.project_ids,
            estimated_spans: started.estimated_spans,
            estimated_bytes: started.estimated_bytes,
            window_from: started.window_from,
            window_to: started.window_to,
        };
        // Audit failures never fail the operation they describe — and here the
        // operation is already queued and about to spend money, so failing it
        // is not even available.
        if let Err(error) = audit_logger.create_audit_log(&event).await {
            tracing::error!(
                %error,
                batch_id = %started.batch_id,
                "failed to record the cloud.telemetry_activation.started audit event; the \
                 activation is running and its spend is still recorded on the job row"
            );
        }
    }))
}

#[utoipa::path(
    post,
    path = "/cloud/backups/source/reconcile",
    tag = "Cloud",
    responses((status = 200, body = ManagedBackupSetup)),
    security(("bearer_auth" = []))
)]
async fn reconcile_cloud_backup_source(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<ManagedBackupSetup>, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, BackupsWrite);
    let result = state
        .service
        .reconcile_managed_backup_source()
        .await
        .map_err(problem)?;
    audit(
        &state,
        &auth,
        &metadata,
        "CLOUD_BACKUP_SOURCE_RECONCILED",
        None,
        None,
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(delete, path = "/cloud", tag = "Cloud", responses((status = 200, body = CloudStatus)), security(("bearer_auth" = [])))]
async fn disconnect_cloud(
    RequireAuth(auth): RequireAuth,
    State(state): State<CloudState>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<CloudStatus>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let (result, backup_credential_revoked) = state.service.disconnect().await.map_err(problem)?;
    audit(
        &state,
        &auth,
        &metadata,
        "CLOUD_LINK_DISCONNECTED",
        None,
        None,
    )
    .await;
    if backup_credential_revoked {
        audit(
            &state,
            &auth,
            &metadata,
            "cloud.backup_credential.revoked",
            None,
            None,
        )
        .await;
    }
    Ok(Json(result))
}

async fn audit(
    state: &CloudState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    action: &'static str,
    previous_features: Option<CloudFeatureSwitches>,
    new_features: Option<CloudFeatureSwitches>,
) {
    let event = CloudLinkAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        action,
        previous_features: previous_features.map(Into::into),
        new_features: new_features.map(Into::into),
    };
    if let Err(error) = state.audit.create_audit_log(&event).await {
        tracing::error!(%error, action, "failed to record managed control-plane audit event");
    }
}

/// The Cloud routes.
///
/// `activation` is optional and has **no route of its own** — it is reachable
/// only from `POST /cloud/enroll`, which is the whole of ADR-042 §9's statement
/// that "the purchase-triggered job is created internally by the enroll path; it
/// has no public POST, because there is no caller for it other than enrollment
/// itself". Everything scoped and operator-initiated goes through
/// `POST /otel/cloud-telemetry/bulk-jobs` and its `plan_token`.
pub fn cloud_routes(
    service: Arc<CloudService>,
    audit: Arc<dyn AuditLogger>,
    activation: Option<Arc<dyn CloudTelemetryActivationTrigger>>,
) -> Router {
    Router::new()
        .route("/cloud/capability", get(get_cloud_capability))
        .route("/cloud/status", get(get_cloud_status))
        .route("/cloud/ai/capability", get(get_cloud_ai_capability))
        .route("/cloud/features", patch(update_cloud_features))
        .route(
            "/cloud/backups/source/reconcile",
            post(reconcile_cloud_backup_source),
        )
        .route("/cloud/enroll", post(enroll_cloud))
        .route("/cloud", delete(disconnect_cloud))
        .with_state(CloudState {
            service,
            audit,
            activation,
        })
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_cloud_capability,
        get_cloud_status,
        get_cloud_ai_capability,
        update_cloud_features,
        reconcile_cloud_backup_source,
        enroll_cloud,
        disconnect_cloud
    ),
    components(schemas(
        CloudAiCapability,
        CloudCapability,
        CloudStatus,
        ManagedBackupSetup,
        ManagedBackupSetupAction,
        ManagedBackupSetupStatus,
        CloudFeatureSwitchesRequest,
        EnrollCloudRequest
    ))
)]
pub struct CloudApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use temps_core::StartedTelemetryActivation;

    // ── ADR-042 P3: the enroll path's fire-and-forget activation ─────────

    /// Records every audit operation type it is handed, and can be told to fail.
    struct RecordingAudit {
        recorded: Mutex<Vec<String>>,
        fail: bool,
    }

    impl RecordingAudit {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                recorded: Mutex::new(Vec::new()),
                fail,
            })
        }

        fn recorded(&self) -> Vec<String> {
            self.recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    #[temps_core::async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(operation.operation_type());
            if self.fail {
                anyhow::bail!("audit store is unavailable");
            }
            Ok(())
        }
    }

    struct StubTrigger(Result<TelemetryActivationOutcome, &'static str>);

    #[temps_core::async_trait::async_trait]
    impl CloudTelemetryActivationTrigger for StubTrigger {
        async fn start_purchase_activation(
            &self,
        ) -> Result<TelemetryActivationOutcome, Box<dyn std::error::Error + Send + Sync>> {
            match &self.0 {
                Ok(outcome) => Ok(outcome.clone()),
                Err(message) => Err(Box::<dyn std::error::Error + Send + Sync>::from(
                    message.to_string(),
                )),
            }
        }
    }

    fn context() -> AuditContext {
        AuditContext {
            user_id: 42,
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: "test".to_string(),
        }
    }

    fn started() -> TelemetryActivationOutcome {
        TelemetryActivationOutcome::Started(StartedTelemetryActivation {
            batch_id: "6b1f9b6a-0000-4000-8000-000000000000".to_string(),
            project_ids: vec![4, 9],
            estimated_spans: 30_000,
            estimated_bytes: 7_500_000,
            window_from: "2026-06-04T00:00:00Z".to_string(),
            window_to: "2026-09-01T00:00:00Z".to_string(),
        })
    }

    #[tokio::test]
    async fn a_queued_activation_is_audited_with_the_job_id_and_what_it_will_cost() {
        let audit = RecordingAudit::new(false);
        let handle = start_telemetry_activation(
            Some(Arc::new(StubTrigger(Ok(started())))),
            audit.clone(),
            context(),
        )
        .expect("a registered trigger must spawn");
        handle.await.expect("the activation task must not panic");

        assert_eq!(audit.recorded(), vec!["cloud.telemetry_activation.started"]);
    }

    #[tokio::test]
    async fn a_job_creation_failure_never_escapes_the_enrollment() {
        // ADR-042's stated risk, in one assertion: "a bug in job creation could
        // fail an enrollment". By the time this runs the enrollment has already
        // succeeded and its response has gone out, so an error here has nowhere
        // to go and must not try — it is logged and nothing is audited, because
        // nothing was queued.
        let audit = RecordingAudit::new(false);
        let handle = start_telemetry_activation(
            Some(Arc::new(StubTrigger(Err("the job store is unreachable")))),
            audit.clone(),
            context(),
        )
        .expect("a registered trigger must spawn");

        handle
            .await
            .expect("a failed activation must not panic the task");
        assert!(
            audit.recorded().is_empty(),
            "nothing was queued, so nothing may claim an activation started"
        );
    }

    #[tokio::test]
    async fn an_audit_write_failure_does_not_stop_the_activation_that_is_already_running() {
        // CLAUDE.md's resilience rule, applied where it matters most: by this
        // point the job row exists and the worker is about to spend money.
        // Failing here could not undo that even if it tried.
        let audit = RecordingAudit::new(true);
        let handle = start_telemetry_activation(
            Some(Arc::new(StubTrigger(Ok(started())))),
            audit.clone(),
            context(),
        )
        .expect("a registered trigger must spawn");

        handle
            .await
            .expect("an audit failure must not panic the task");
        assert_eq!(audit.recorded(), vec!["cloud.telemetry_activation.started"]);
    }

    #[tokio::test]
    async fn a_skipped_activation_audits_nothing_because_no_spend_was_authorized() {
        // Every skip is a state, not an event: no job exists, nothing will be
        // billed, and an audit row claiming an activation started would be false.
        for skipped in [
            TelemetryActivationSkipped::NoLocalProjects,
            TelemetryActivationSkipped::NotConfigured {
                reason: "telemetry export is switched off".to_string(),
                setup_path: Some("/settings/cloud".to_string()),
            },
            TelemetryActivationSkipped::AlreadyActive {
                batch_id: "6b1f9b6a-0000-4000-8000-000000000000".to_string(),
            },
        ] {
            let audit = RecordingAudit::new(false);
            let handle = start_telemetry_activation(
                Some(Arc::new(StubTrigger(Ok(
                    TelemetryActivationOutcome::Skipped(skipped.clone()),
                )))),
                audit.clone(),
                context(),
            )
            .expect("a registered trigger must spawn");
            handle.await.expect("the task must not panic");

            assert!(
                audit.recorded().is_empty(),
                "{skipped:?} must audit nothing"
            );
        }
    }

    #[tokio::test]
    async fn an_instance_with_no_activation_trigger_enrolls_exactly_as_before() {
        // The regression guard for the coupling this phase adds: a build with no
        // OTel plugin, or an instance with no local span source, must reach the
        // end of enroll having done nothing extra at all.
        let audit = RecordingAudit::new(false);
        assert!(
            start_telemetry_activation(None, audit.clone(), context()).is_none(),
            "no trigger means no task"
        );
        assert!(audit.recorded().is_empty());
    }

    // ── The two gates on the auto-spend (ADR-042 review, Findings 3 & 4) ──

    fn test_user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 42,
            name: "Operator".to_string(),
            email: "operator@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn principal(role: temps_auth::Role) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_session(test_user(), role)
    }

    fn first_enrollment() -> temps_cloud_client::EnrollmentKind {
        temps_cloud_client::EnrollmentKind::First
    }

    fn re_enrollment() -> temps_cloud_client::EnrollmentKind {
        temps_cloud_client::EnrollmentKind::ReEnrolled {
            tenant_id: uuid::Uuid::new_v4(),
        }
    }

    #[test]
    fn a_fresh_enrollment_by_an_instance_admin_still_queues_the_activation() {
        // The behaviour ADR-042 P3 shipped, and the one both gates must leave
        // intact: a customer who has just paid gets their history shipped
        // without having to find a second button.
        for role in [temps_auth::Role::Admin, temps_auth::Role::PlatformAdmin] {
            assert!(
                may_start_activation(&principal(role.clone()), first_enrollment()),
                "{role} enrolling a fresh instance must queue an activation"
            );
        }
    }

    #[test]
    fn re_enrolling_the_same_tenant_does_not_re_trigger_an_activation() {
        // Finding 3. `CloudLink::enroll` overwrites the credential on an
        // already-linked instance on purpose — that is credential recovery after
        // `CredentialRejected`. Treating it as a purchase would silently
        // re-activate every project the operator deliberately kept local after
        // the first activation, spending their money to undo their own decision.
        assert!(
            !may_start_activation(&principal(temps_auth::Role::Admin), re_enrollment()),
            "a credential recovery is not a purchase"
        );
    }

    #[test]
    fn enrolling_a_different_tenant_is_a_new_link_and_does_trigger_one() {
        // The other half of Finding 3: a stale credential from a previous owner
        // must not deny a genuinely new customer the activation they paid for.
        let rebound = temps_cloud_client::EnrollmentKind::ReboundToNewTenant {
            previous_tenant_id: Some(uuid::Uuid::new_v4()),
        };
        assert!(may_start_activation(
            &principal(temps_auth::Role::Admin),
            rebound
        ));
    }

    #[test]
    fn a_settings_write_only_principal_enrolls_without_queueing_a_spend() {
        // Finding 4. The operator path to the same engine requires `OtelWrite`
        // *and* instance admin; reaching the identical spend through
        // `POST /cloud/enroll` with only `SettingsWrite` would make that bar
        // decorative. Enrollment itself is untouched — this predicate governs
        // only the side effect — so the link, the feature switches and the
        // managed backup source are all still established.
        for role in [
            temps_auth::Role::User,
            temps_auth::Role::Reader,
            temps_auth::Role::Custom,
        ] {
            assert!(
                !may_start_activation(&principal(role.clone()), first_enrollment()),
                "{role} must not be able to auto-start an instance-wide spend"
            );
        }

        // A project-scoped machine credential least of all.
        let token = temps_auth::AuthContext::new_deployment_token(
            7,
            None,
            None,
            1,
            "deploy-token".to_string(),
            Vec::new(),
        );
        assert!(!may_start_activation(&token, first_enrollment()));
    }

    #[test]
    fn the_started_audit_records_the_spend_and_names_the_purchase_trigger() {
        let event = CloudTelemetryActivationStartedAudit {
            context: context(),
            batch_id: "6b1f9b6a-0000-4000-8000-000000000000".to_string(),
            trigger: "purchase",
            project_ids: vec![4, 9],
            project_count: 2,
            estimated_spans: 30_000,
            estimated_bytes: 7_500_000,
            window_from: "2026-06-04T00:00:00Z".to_string(),
            window_to: "2026-09-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            AuditOperation::operation_type(&event),
            "cloud.telemetry_activation.started",
            "the dotted action name ADR-042 P3 names, matching \
             cloud.backup_credential.issued on the same handler"
        );
        let serialized = AuditOperation::serialize(&event).expect("must serialize");
        // A customer disputing an invoice needs the pre-send estimate and the
        // exact project set on the record, not just the fact that it happened.
        assert!(
            serialized.contains("\"trigger\":\"purchase\""),
            "{serialized}"
        );
        assert!(
            serialized.contains("\"estimated_bytes\":7500000"),
            "{serialized}"
        );
        assert!(serialized.contains("\"project_ids\":[4,9]"), "{serialized}");
        assert!(
            serialized.contains("6b1f9b6a-0000-4000-8000-000000000000"),
            "{serialized}"
        );
    }

    #[test]
    fn feature_audit_contains_before_and_after_consent() {
        let audit = CloudLinkAudit {
            context: AuditContext {
                user_id: 42,
                ip_address: Some("127.0.0.1".to_string()),
                user_agent: "test".to_string(),
            },
            action: "CLOUD_FEATURES_UPDATED",
            previous_features: Some(
                CloudFeatureSwitches {
                    telemetry: false,
                    backups: false,
                    notifications: false,
                }
                .into(),
            ),
            new_features: Some(
                CloudFeatureSwitches {
                    telemetry: true,
                    backups: true,
                    notifications: false,
                }
                .into(),
            ),
        };
        let serialized = AuditOperation::serialize(&audit).unwrap();
        assert!(serialized.contains("\"previous_features\""));
        assert!(serialized.contains("\"new_features\""));
        assert!(serialized.contains("\"backups_enabled\":true"));
    }

    #[test]
    fn test_cloud_internal_failure_problem_hides_configuration_details() {
        let secret = "postgres://operator:do-not-leak@internal.example.invalid/cloud";

        let response = problem(CloudServiceError::InvalidBackend {
            reason: format!("could not connect to {secret}"),
        });

        assert_eq!(response.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let detail = response
            .body
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .expect("problem detail");
        assert!(!detail.contains(secret));
        assert!(!detail.contains("do-not-leak"));
        assert!(detail.contains("server logs"));
    }

    #[test]
    fn test_cloud_upstream_failure_problem_hides_provider_response() {
        let provider_detail = "object storage signature included secret-token";

        let response = problem(CloudServiceError::Client(
            temps_cloud_client::CloudError::Rejected {
                detail: provider_detail.into(),
            },
        ));

        assert_eq!(response.status_code, StatusCode::BAD_GATEWAY);
        let detail = response
            .body
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .expect("problem detail");
        assert!(!detail.contains(provider_detail));
        assert!(!detail.contains("secret-token"));
    }

    #[test]
    fn test_cloud_upload_idle_timeout_is_safe_and_retryable() {
        let error = temps_cloud_client::CloudError::BackupUploadIdleTimeout {
            idle_timeout_ms: 60_000,
            spooled_bytes: 42,
        };
        assert!(error.is_retryable());

        let response = problem(CloudServiceError::Client(error));

        assert_eq!(response.status_code, StatusCode::SERVICE_UNAVAILABLE);
        let detail = response
            .body
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .expect("problem detail");
        assert!(!detail.contains("60000"));
        assert!(!detail.contains("42"));
        assert!(detail.contains("server logs"));
    }
}
