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
    AuditOperation, RequestMetadata,
};
use utoipa::{OpenApi, ToSchema};

use crate::{
    CloudAiCapability, CloudCapability, CloudService, CloudServiceError, CloudStatus,
    ManagedBackupOutcome,
};
use temps_cloud_client::CloudFeatureSwitches;

#[derive(Clone)]
pub struct CloudState {
    service: Arc<CloudService>,
    audit: Arc<dyn AuditLogger>,
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
    let (result, backup_outcome) = state
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
        ManagedBackupOutcome::NotConfigured | ManagedBackupOutcome::Unavailable(_) => {}
    }
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

pub fn cloud_routes(service: Arc<CloudService>, audit: Arc<dyn AuditLogger>) -> Router {
    Router::new()
        .route("/cloud/capability", get(get_cloud_capability))
        .route("/cloud/status", get(get_cloud_status))
        .route("/cloud/ai/capability", get(get_cloud_ai_capability))
        .route("/cloud/features", patch(update_cloud_features))
        .route("/cloud/enroll", post(enroll_cloud))
        .route("/cloud", delete(disconnect_cloud))
        .with_state(CloudState { service, audit })
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_cloud_capability,
        get_cloud_status,
        get_cloud_ai_capability,
        update_cloud_features,
        enroll_cloud,
        disconnect_cloud
    ),
    components(schemas(
        CloudAiCapability,
        CloudCapability,
        CloudStatus,
        CloudFeatureSwitchesRequest,
        EnrollCloudRequest
    ))
)]
pub struct CloudApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

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
