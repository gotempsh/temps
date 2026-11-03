// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext, AuditOperation, RequestMetadata,
};
use utoipa::{IntoParams, OpenApi};

pub struct CronMonitorsState {
    pub service: Arc<CronMonitorService>,
    pub alarms: Arc<crate::AlarmService>,
    pub audit: Arc<dyn temps_core::AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}
impl From<CronMonitorError> for Problem {
    fn from(error: CronMonitorError) -> Self {
        let status = match &error {
            CronMonitorError::Invalid(_) => StatusCode::BAD_REQUEST,
            CronMonitorError::NotFound { .. } | CronMonitorError::ScopeNotFound { .. } => {
                StatusCode::NOT_FOUND
            }
            CronMonitorError::Conflict => StatusCode::CONFLICT,
            CronMonitorError::Limit => StatusCode::TOO_MANY_REQUESTS,
            CronMonitorError::Database(_)
            | CronMonitorError::StoredConfiguration { .. }
            | CronMonitorError::Alarm(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(error = %error, "Cron monitor operation failed");
        }
        let detail = match &error {
            CronMonitorError::Database(_) | CronMonitorError::Alarm(_) => {
                "Cron monitor operation failed; check the server logs for details".into()
            }
            _ => error.to_string(),
        };
        problemdetails::new(status)
            .with_title("Cron monitor operation failed")
            .with_detail(detail)
    }
}
#[derive(Deserialize, IntoParams)]
pub struct CronMonitorQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}
#[derive(Serialize)]
struct CronAudit {
    context: AuditContext,
    project_id: i32,
    monitor_id: i32,
    operation: &'static str,
}
impl AuditOperation for CronAudit {
    fn operation_type(&self) -> String {
        self.operation.into()
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}
async fn audit(
    state: &CronMonitorsState,
    user_id: i32,
    metadata: RequestMetadata,
    project_id: i32,
    monitor_id: i32,
    operation: &'static str,
) {
    let event = CronAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        monitor_id,
        operation,
    };
    if let Err(error) = state.audit.create_audit_log(&event).await {
        tracing::error!(project_id,monitor_id,operation,error=%error,"Could not record cron monitor audit event");
    }
}

#[utoipa::path(get,path="/projects/{project_id}/cron-monitors",tag="Cron Monitors",operation_id="listCronMonitors",params(("project_id"=i32,Path,description="Project ID"),CronMonitorQuery),
responses((status=200,description="Success",body=CronMonitorList),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn list(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<CronMonitorQuery>,
) -> Result<Json<CronMonitorList>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state
        .service
        .list_monitors(
            project_id,
            query.page.unwrap_or(1),
            query.page_size.unwrap_or(20),
        )
        .await?;
    Ok(Json(result))
}

#[utoipa::path(post,path="/projects/{project_id}/cron-monitors",tag="Cron Monitors",operation_id="createCronMonitor",params(("project_id"=i32,Path,description="Project ID")),
request_body=CreateCronMonitor,
responses((status=201,description="Success",body=CronMonitorView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn create(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(input): Json<CreateCronMonitor>,
) -> Result<(StatusCode, Json<CronMonitorView>), Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state.service.create_monitor(project_id, input).await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        result.id,
        "CRON_MONITOR_CREATED",
    )
    .await;
    Ok((StatusCode::CREATED, Json(result)))
}

#[utoipa::path(get,path="/projects/{project_id}/cron-monitors/{monitor_id}",tag="Cron Monitors",operation_id="getCronMonitor",params(("project_id"=i32,Path,description="Project ID"),("monitor_id"=i32,Path,description="Monitor ID")),
responses((status=200,description="Success",body=CronMonitorView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn get_one(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path((project_id, monitor_id)): Path<(i32, i32)>,
) -> Result<Json<CronMonitorView>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state.service.get_monitor(project_id, monitor_id).await?;
    Ok(Json(result))
}

#[utoipa::path(patch,path="/projects/{project_id}/cron-monitors/{monitor_id}",tag="Cron Monitors",operation_id="updateCronMonitor",params(("project_id"=i32,Path,description="Project ID"),("monitor_id"=i32,Path,description="Monitor ID")),
request_body=UpdateCronMonitor,
responses((status=200,description="Success",body=CronMonitorView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn update(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path((project_id, monitor_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(input): Json<UpdateCronMonitor>,
) -> Result<Json<CronMonitorView>, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state
        .service
        .update_monitor(project_id, monitor_id, input)
        .await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        monitor_id,
        "CRON_MONITOR_UPDATED",
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(delete,path="/projects/{project_id}/cron-monitors/{monitor_id}",tag="Cron Monitors",operation_id="deleteCronMonitor",params(("project_id"=i32,Path,description="Project ID"),("monitor_id"=i32,Path,description="Monitor ID")),
responses((status=204,description="Success"),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn delete(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path((project_id, monitor_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state
        .service
        .delete_monitor(project_id, monitor_id, &state.alarms)
        .await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        monitor_id,
        "CRON_MONITOR_DELETED",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/projects/{project_id}/cron-monitors/{monitor_id}/runs",tag="Cron Monitors",operation_id="listCronMonitorRuns",params(("project_id"=i32,Path,description="Project ID"),("monitor_id"=i32,Path,description="Monitor ID"),CronMonitorQuery),
responses((status=200,description="Success",body=CronMonitorRunList),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Already exists"),(status=429,description="Monitor limit reached"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn runs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CronMonitorsState>>,
    Path((project_id, monitor_id)): Path<(i32, i32)>,
    Query(query): Query<CronMonitorQuery>,
) -> Result<Json<CronMonitorRunList>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state
        .service
        .list_monitor_runs(
            project_id,
            monitor_id,
            query.page.unwrap_or(1),
            query.page_size.unwrap_or(20),
        )
        .await?;
    Ok(Json(result))
}

pub fn routes() -> Router<Arc<CronMonitorsState>> {
    Router::new()
        .route(
            "/projects/{project_id}/cron-monitors",
            get(list).post(create),
        )
        .route(
            "/projects/{project_id}/cron-monitors/{monitor_id}",
            get(get_one).patch(update).delete(delete),
        )
        .route(
            "/projects/{project_id}/cron-monitors/{monitor_id}/runs",
            get(runs),
        )
}
#[derive(OpenApi)]
#[openapi(
    paths(list, create, get_one, update, delete, runs),
    components(schemas(
        CreateCronMonitor,
        UpdateCronMonitor,
        CronMonitorView,
        CronMonitorList,
        CronMonitorRunList,
        CronMonitorRun,
        MonitorConfig,
        MonitorSchedule
    ))
)]
pub struct CronMonitorsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tower::ServiceExt;
    #[tokio::test]
    async fn storage_and_alarm_causes_do_not_escape_http_problems() {
        use axum::response::IntoResponse;
        for error in [
            CronMonitorError::from(DbErr::Custom("synthetic internal storage detail".into())),
            CronMonitorError::from(crate::AlarmError::Database {
                operation: "synthetic internal alarm operation".into(),
                reason: "synthetic internal alarm detail".into(),
            }),
        ] {
            let response = Problem::from(error).into_response();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let detail = body["detail"].as_str().unwrap();
            assert!(detail.contains("server logs"));
            assert!(!detail.contains("synthetic internal"));
        }
    }
    fn session(role: temps_authz::Role) -> temps_authz::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test".into(),
            email: "test@example.com".into(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        temps_authz::AuthContext::new_session(user, role)
    }

    struct NoAudit;
    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(&self, _: &dyn AuditOperation) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }
    struct Denied;
    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for Denied {
        async fn user_can_access_project(
            &self,
            _: i32,
            _: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }
    async fn call(auth: Option<temps_authz::AuthContext>, method: &str, uri: &str) -> StatusCode {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let state = Arc::new(CronMonitorsState {
            service: Arc::new(CronMonitorService::new(db.clone())),
            alarms: Arc::new(crate::AlarmService::new(
                db,
                Arc::new(super::super::tests::Notifications),
                Arc::new(super::super::tests::Queue),
            )),
            audit: Arc::new(NoAudit),
            project_access_checker: Some(Arc::new(Denied)),
        });
        let body = if method == "POST" {
            r#"{"slug":"synthetic-job","config":{"schedule":{"type":"interval","value":1,"unit":"hour"}}}"#
        } else {
            r#"{"enabled":false}"#
        };
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        request.extensions_mut().insert(RequestMetadata {
            ip_address: "127.0.0.1".into(),
            user_agent: "test".into(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        });
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        routes()
            .with_state(state)
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }
    #[tokio::test]
    async fn management_routes_require_auth_permissions_and_project_access() {
        for (method, uri) in [
            ("GET", "/projects/1/cron-monitors"),
            ("POST", "/projects/1/cron-monitors"),
            ("GET", "/projects/1/cron-monitors/2"),
            ("PATCH", "/projects/1/cron-monitors/2"),
            ("DELETE", "/projects/1/cron-monitors/2"),
            ("GET", "/projects/1/cron-monitors/2/runs"),
        ] {
            assert_eq!(
                call(None, method, uri).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
            assert_eq!(
                call(Some(session(temps_authz::Role::Reader)), method, uri).await,
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
            assert_eq!(
                call(Some(session(temps_authz::Role::User)), method, uri).await,
                StatusCode::FORBIDDEN,
                "project membership: {method} {uri}"
            );
        }
        let auth = temps_authz::AuthContext::new_deployment_token(
            2,
            None,
            None,
            1,
            "synthetic".into(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
        );
        assert_eq!(
            call(Some(auth), "GET", "/projects/1/cron-monitors").await,
            StatusCode::FORBIDDEN
        );
    }
}
