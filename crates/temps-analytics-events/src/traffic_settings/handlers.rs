// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for per-project analytics traffic settings.
//!
//! The response always carries the datacenter-detection status next to the
//! setting, so the console can tell "exclusion is on and working" from
//! "exclusion is on but no ASN database is loaded, so nothing is being
//! detected" -- the second case must render as an onboarding state, never as a
//! silently ineffective toggle.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditLogger, AuditOperation, RequestMetadata};
use temps_geo::GeoIpService;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::filters::TrafficFilterService;
use super::service::{
    ProjectAnalyticsSettings, ProjectAnalyticsSettingsError, ProjectAnalyticsSettingsPatch,
    ProjectAnalyticsSettingsService,
};
use super::spam_referrers::spam_referrer_list_size;

/// Console page where the operator manages the geolocation databases.
const GEO_SETTINGS_PATH: &str = "/settings/metrics-monitoring";

/// State for the analytics traffic-settings and traffic-filter routes. Each
/// dependency is its own `Arc` clone; the settings and filter services are the
/// *same* instances the ingest path reads, so a write here takes effect on
/// this process immediately.
pub struct TrafficSettingsAppState {
    pub settings_service: Arc<ProjectAnalyticsSettingsService>,
    pub filter_service: Arc<TrafficFilterService>,
    pub geo_ip_service: Arc<GeoIpService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

/// Whether datacenter traffic can be detected on this instance at all.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct DatacenterDetectionStatus {
    /// `true` when an ASN database is loaded and IPs are being classified.
    pub available: bool,
    /// Why detection is unavailable, when `available` is false.
    pub reason: Option<String>,
    /// Console path where the operator can fix it, when `available` is false.
    pub setup_path: Option<String>,
    /// When the loaded ASN database was built (ISO 8601), if one is loaded.
    pub asn_database_built_at: Option<String>,
}

impl DatacenterDetectionStatus {
    fn from_geo(geo: &GeoIpService) -> Self {
        if geo.has_asn_database() {
            return Self {
                available: true,
                reason: None,
                setup_path: None,
                asn_database_built_at: geo
                    .asn_build_epoch()
                    .and_then(|epoch| i64::try_from(epoch).ok())
                    .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
                    .map(|built| built.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            };
        }
        Self {
            available: false,
            reason: Some(
                "No ASN database is loaded, so visitors cannot be classified as datacenter \
                 traffic. Temps ships GeoLite2-ASN.mmdb next to GeoLite2-City.mmdb; place it \
                 in the working or data directory and restart Temps."
                    .to_string(),
            ),
            setup_path: Some(GEO_SETTINGS_PATH.to_string()),
            asn_database_built_at: None,
        }
    }
}

/// Analytics traffic settings for a project.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ProjectAnalyticsSettingsResponse {
    pub project_id: i32,
    /// Exclude visits from hosting/cloud/VPS networks (AWS, Google Cloud,
    /// Tencent, Alibaba, Hetzner, ...) from every analytics report. Privacy
    /// relays such as iCloud Private Relay are never treated as datacenters.
    pub exclude_datacenter_traffic: bool,
    pub datacenter_detection: DatacenterDetectionStatus,
    /// Exclude visits whose referrer is on the bundled referrer-spam list
    /// (domains that exist only to plant fake referrals).
    pub exclude_spam_referrers: bool,
    /// Number of domains on the bundled referrer-spam list.
    pub spam_referrer_list_size: u32,
    /// When the settings were last changed (ISO 8601). `null` while the
    /// project still uses the defaults.
    pub updated_at: Option<String>,
}

impl ProjectAnalyticsSettingsResponse {
    fn new(settings: ProjectAnalyticsSettings, geo: &GeoIpService) -> Self {
        Self {
            project_id: settings.project_id,
            exclude_datacenter_traffic: settings.exclude_datacenter_traffic,
            datacenter_detection: DatacenterDetectionStatus::from_geo(geo),
            exclude_spam_referrers: settings.exclude_spam_referrers,
            spam_referrer_list_size: u32::try_from(spam_referrer_list_size()).unwrap_or(u32::MAX),
            updated_at: settings
                .updated_at
                .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        }
    }
}

/// Fields to change. Omitted fields keep their current value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateProjectAnalyticsSettingsRequest {
    pub exclude_datacenter_traffic: Option<bool>,
    pub exclude_spam_referrers: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectAnalyticsSettingsUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub previous_exclude_datacenter_traffic: bool,
    pub exclude_datacenter_traffic: bool,
    pub previous_exclude_spam_referrers: bool,
    pub exclude_spam_referrers: bool,
}

impl AuditOperation for ProjectAnalyticsSettingsUpdatedAudit {
    fn operation_type(&self) -> String {
        "PROJECT_ANALYTICS_SETTINGS_UPDATED".to_string()
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
        serde_json::to_string(self).map_err(|e| {
            anyhow::anyhow!(
                "Failed to serialize PROJECT_ANALYTICS_SETTINGS_UPDATED audit for project {}: {}",
                self.project_id,
                e
            )
        })
    }
}

impl From<ProjectAnalyticsSettingsError> for Problem {
    fn from(error: ProjectAnalyticsSettingsError) -> Self {
        match error {
            ProjectAnalyticsSettingsError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            ProjectAnalyticsSettingsError::Database { .. } => {
                error!("{error}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(get_project_analytics_settings, update_project_analytics_settings),
    components(schemas(
        ProjectAnalyticsSettingsResponse,
        DatacenterDetectionStatus,
        UpdateProjectAnalyticsSettingsRequest,
    )),
    tags(
        (name = "Analytics Settings", description = "Per-project analytics traffic settings")
    )
)]
pub struct TrafficSettingsApiDoc;

pub fn configure_traffic_settings_routes() -> Router<Arc<TrafficSettingsAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/analytics/settings",
            get(get_project_analytics_settings).patch(update_project_analytics_settings),
        )
        .merge(super::filter_handlers::configure_traffic_filter_routes())
}

/// Get a project's analytics traffic settings and whether datacenter
/// detection is available on this instance.
#[utoipa::path(
    tag = "Analytics Settings",
    get,
    path = "/projects/{project_id}/analytics/settings",
    operation_id = "get_project_analytics_settings",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Analytics settings", body = ProjectAnalyticsSettingsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_project_analytics_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let settings = state.settings_service.get(project_id).await?;
    Ok(Json(ProjectAnalyticsSettingsResponse::new(
        settings,
        &state.geo_ip_service,
    )))
}

/// Change a project's analytics traffic settings. Applies to events ingested
/// from now on; already-recorded events keep their classification.
#[utoipa::path(
    tag = "Analytics Settings",
    patch,
    path = "/projects/{project_id}/analytics/settings",
    operation_id = "update_project_analytics_settings",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = UpdateProjectAnalyticsSettingsRequest,
    responses(
        (status = 200, description = "Updated analytics settings", body = ProjectAnalyticsSettingsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_project_analytics_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<TrafficSettingsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<UpdateProjectAnalyticsSettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let previous = state.settings_service.get(project_id).await?;
    let settings = state
        .settings_service
        .update(
            project_id,
            ProjectAnalyticsSettingsPatch {
                exclude_datacenter_traffic: request.exclude_datacenter_traffic,
                exclude_spam_referrers: request.exclude_spam_referrers,
            },
        )
        .await?;

    let audit = ProjectAnalyticsSettingsUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        previous_exclude_datacenter_traffic: previous.exclude_datacenter_traffic,
        exclude_datacenter_traffic: settings.exclude_datacenter_traffic,
        previous_exclude_spam_referrers: previous.exclude_spam_referrers,
        exclude_spam_referrers: settings.exclude_spam_referrers,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create analytics settings audit log for project {}: {}",
            project_id, e
        );
    }

    Ok(Json(ProjectAnalyticsSettingsResponse::new(
        settings,
        &state.geo_ip_service,
    )))
}

#[cfg(test)]
pub(in crate::traffic_settings) mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::{project_analytics_settings, users};

    #[derive(Default)]
    pub(in crate::traffic_settings) struct RecordingAuditLogger {
        operations: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let mut operations = match self.operations.lock() {
                Ok(operations) => operations,
                Err(poisoned) => poisoned.into_inner(),
            };
            operations.push(operation.operation_type());
            Ok(())
        }
    }

    impl RecordingAuditLogger {
        pub(in crate::traffic_settings) fn recorded(&self) -> Vec<String> {
            match self.operations.lock() {
                Ok(operations) => operations.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    fn user_model(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
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

    fn settings_row(project_id: i32, exclude: bool) -> project_analytics_settings::Model {
        let now = Utc::now();
        project_analytics_settings::Model {
            project_id,
            exclude_datacenter_traffic: exclude,
            exclude_spam_referrers: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub(in crate::traffic_settings) fn state_with(
        db: MockDatabase,
    ) -> (Arc<TrafficSettingsAppState>, Arc<RecordingAuditLogger>) {
        let audit = Arc::new(RecordingAuditLogger::default());
        let db = Arc::new(db.into_connection());
        let state = Arc::new(TrafficSettingsAppState {
            settings_service: Arc::new(ProjectAnalyticsSettingsService::new(db.clone())),
            filter_service: Arc::new(TrafficFilterService::new(db)),
            // Mock mode loads no ASN database, which is exactly the
            // "detection unavailable" state worth asserting on.
            geo_ip_service: Arc::new(GeoIpService::Mock(temps_geo::MockGeoIpService::new())),
            audit_service: audit.clone(),
            project_access_checker: None,
        });
        (state, audit)
    }

    pub(in crate::traffic_settings) fn auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(user_model(1), role))
    }

    pub(in crate::traffic_settings) fn metadata() -> Extension<RequestMetadata> {
        Extension(RequestMetadata {
            ip_address: "203.0.113.7".to_string(),
            user_agent: "test-agent".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.com".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.com".to_string(),
            is_secure: true,
        })
    }

    async fn body(response: axum::response::Response) -> ProjectAnalyticsSettingsResponse {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("readable body");
        serde_json::from_slice(&bytes).expect("settings response JSON")
    }

    #[tokio::test]
    async fn get_returns_the_setting_and_an_onboarding_status_without_an_asn_database() {
        let (state, _) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![settings_row(1, true)]]),
        );

        let response = get_project_analytics_settings(auth(Role::Admin), State(state), Path(1))
            .await
            .expect("admin can read settings")
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let settings = body(response).await;
        assert!(settings.exclude_datacenter_traffic);
        assert!(!settings.datacenter_detection.available);
        assert!(settings
            .datacenter_detection
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("No ASN database")));
        assert_eq!(
            settings.datacenter_detection.setup_path.as_deref(),
            Some(GEO_SETTINGS_PATH)
        );
    }

    #[tokio::test]
    async fn get_of_an_unknown_project_is_a_404_problem() {
        let (state, _) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<project_analytics_settings::Model>::new()])
                .append_query_results([
                    Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new(),
                ]),
        );

        let err = get_project_analytics_settings(auth(Role::Admin), State(state), Path(404))
            .await
            .err()
            .expect("unknown project");
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_requires_analytics_read() {
        let (state, _) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = get_project_analytics_settings(auth(Role::MetricsIngest), State(state), Path(1))
            .await
            .err()
            .expect("a role without AnalyticsRead is rejected");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_requires_analytics_write() {
        let (state, audit) = state_with(MockDatabase::new(DatabaseBackend::Postgres));
        let err = update_project_analytics_settings(
            auth(Role::ApiReader),
            State(state),
            metadata(),
            Path(1),
            Json(UpdateProjectAnalyticsSettingsRequest {
                exclude_datacenter_traffic: Some(false),
                exclude_spam_referrers: None,
            }),
        )
        .await
        .err()
        .expect("a reader cannot change settings");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn update_saves_the_setting_and_writes_an_audit_entry() {
        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                // handler: read previous value
                .append_query_results([vec![settings_row(1, true)]])
                // service.update: get()
                .append_query_results([vec![settings_row(1, true)]])
                // upsert returning
                .append_query_results([vec![settings_row(1, false)]]),
        );

        let response = update_project_analytics_settings(
            auth(Role::Admin),
            State(state),
            metadata(),
            Path(1),
            Json(UpdateProjectAnalyticsSettingsRequest {
                exclude_datacenter_traffic: Some(false),
                exclude_spam_referrers: None,
            }),
        )
        .await
        .expect("admin can change settings")
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!body(response).await.exclude_datacenter_traffic);
        assert_eq!(audit.recorded(), vec!["PROJECT_ANALYTICS_SETTINGS_UPDATED"]);
    }
}
