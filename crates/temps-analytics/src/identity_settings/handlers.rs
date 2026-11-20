// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET` and `PUT /projects/{project_id}/analytics/identity`.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::Serialize;
use temps_authz::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem},
    AuditLogger, AuditOperation, RequestMetadata,
};
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use utoipa::OpenApi;

use super::service::AnalyticsIdentityService;
use super::types::*;

pub struct AnalyticsIdentityState {
    pub service: Arc<AnalyticsIdentityService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(get_analytics_identity_settings, update_analytics_identity_settings),
    components(schemas(
        AnalyticsIdentitySettings,
        UpdateAnalyticsIdentitySettings,
        AnalyticsIdentityMode
    )),
    tags((name = "Analytics", description = "Web analytics"))
)]
pub struct AnalyticsIdentityApiDoc;

pub fn routes() -> Router<Arc<AnalyticsIdentityState>> {
    Router::new().route(
        "/projects/{project_id}/analytics/identity",
        get(get_analytics_identity_settings).put(update_analytics_identity_settings),
    )
}

impl From<AnalyticsIdentityError> for Problem {
    fn from(error: AnalyticsIdentityError) -> Self {
        match &error {
            AnalyticsIdentityError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            AnalyticsIdentityError::Database { .. } => {
                tracing::error!(error = %error, "Analytics identity settings request failed");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Serialize)]
struct IdentityModeChangedAudit {
    project_id: i32,
    previous_mode: AnalyticsIdentityMode,
    mode: AnalyticsIdentityMode,
    user_id: Option<i32>,
    ip_address: String,
    user_agent: String,
}

impl AuditOperation for IdentityModeChangedAudit {
    fn operation_type(&self) -> String {
        "ANALYTICS_IDENTITY_MODE_UPDATED".to_string()
    }
    fn user_id(&self) -> Option<i32> {
        self.user_id
    }
    fn ip_address(&self) -> Option<String> {
        Some(self.ip_address.clone())
    }
    fn user_agent(&self) -> &str {
        &self.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// How the project identifies visitors: stored ids or cookieless
#[utoipa::path(
    get,
    path = "/projects/{project_id}/analytics/identity",
    tag = "Analytics",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Identity settings", body = AnalyticsIdentitySettings),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_analytics_identity_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIdentityState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<AnalyticsIdentitySettings>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(state.service.get(project_id).await?))
}

/// Switch between stored visitor ids and cookieless tracking
///
/// `cookieless` stores nothing on the visitor's device: the proxy stops
/// setting analytics cookies and visitors are counted by a hash that changes
/// every day. Visitors already counted keep their history.
#[utoipa::path(
    put,
    path = "/projects/{project_id}/analytics/identity",
    tag = "Analytics",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = UpdateAnalyticsIdentitySettings,
    responses(
        (status = 200, description = "Identity settings updated", body = AnalyticsIdentitySettings),
        (status = 400, description = "Unknown mode"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_analytics_identity_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIdentityState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<UpdateAnalyticsIdentitySettings>,
) -> Result<Json<AnalyticsIdentitySettings>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let change = state.service.set(project_id, request.mode).await?;
    if change.previous != change.settings.mode {
        let audit = IdentityModeChangedAudit {
            project_id,
            previous_mode: change.previous,
            mode: change.settings.mode,
            user_id: auth.user_id_opt(),
            ip_address: metadata.ip_address,
            user_agent: metadata.user_agent,
        };
        if let Err(error) = state.audit.create_audit_log(&audit).await {
            tracing::error!(
                project_id,
                error = %error,
                "Failed to audit analytics identity mode change"
            );
        }
    }
    Ok(Json(change.settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_their_http_status() {
        let not_found = Problem::from(AnalyticsIdentityError::ProjectNotFound { project_id: 4 });
        assert_eq!(not_found.status_code, StatusCode::NOT_FOUND);
        let body = serde_json::to_value(&not_found.body).expect("problem body");
        assert_eq!(body["detail"], "Project 4 not found");

        let db = Problem::from(AnalyticsIdentityError::Database {
            project_id: 4,
            operation: "load project identity settings",
            source: sea_orm::DbErr::Custom("down".into()),
        });
        assert_eq!(db.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn cookieless_settings_name_the_sdk_option_and_the_limits() {
        let now = chrono::Utc::now();
        let cookieless = AnalyticsIdentitySettings::new(1, AnalyticsIdentityMode::Cookieless, now);
        assert_eq!(cookieless.sdk_option.as_deref(), Some("cookieless: true"));
        assert_eq!(cookieless.limitations.len(), COOKIELESS_LIMITATIONS.len());

        let persistent = AnalyticsIdentitySettings::new(1, AnalyticsIdentityMode::Persistent, now);
        assert!(persistent.sdk_option.is_none());
        assert!(persistent.limitations.is_empty());
    }
}
