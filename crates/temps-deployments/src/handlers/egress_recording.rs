// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording an environment's outbound connections: the switch, its
//! capability and the recorded connections.
//!
//! - `GET /projects/{project_id}/environments/{environment_id}/egress-recording`:
//!   whether it records, and whether it can (and if not, what is missing).
//! - `PUT` the same path with `{"enabled": true}`: turn it on or off.
//!   Audit-logged.
//! - `GET /projects/{project_id}/environments/{environment_id}/egress-records`:
//!   the environment's recorded connections, newest first, paginated.
//!
//! These are the environment's URLs and the environment's authorization
//! (the project's `EnvironmentsRead`/`EnvironmentsWrite`). Everything
//! behind them (the flow, the request and response types, the audit entry,
//! the errors) is `temps-workload-egress`, shared with sandboxes.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json, Router};
use temps_auth::{project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_workload_egress::handlers::{
    EgressActivityParams, EgressRecordingUpdatedAudit, SetEgressRecordingBody,
};
use temps_workload_egress::{
    EgressActivityPage, EgressActivityRecord, EgressOutcomeFilter, EgressRecordingNode,
    EgressRecordingStatus,
};
use utoipa::OpenApi;

use crate::services::egress_recording::EnvironmentEgress;

pub struct EnvironmentEgressAppState {
    pub service: Arc<EnvironmentEgress>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Team-based project access, when a plugin provides it.
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_environment_egress_recording,
        set_environment_egress_recording,
        list_environment_egress_records
    ),
    components(schemas(
        EgressRecordingStatus,
        EgressRecordingNode,
        SetEgressRecordingBody,
        EgressActivityPage,
        EgressActivityRecord,
        EgressOutcomeFilter
    ))
)]
pub struct EnvironmentEgressApiDoc;

/// Whether the environment records its outbound connections, and whether
/// it can.
///
/// `configured: false` says recording cannot work for this environment
/// now, with `reason_code`, `reason` and, when the console can fix it,
/// `setup_path`. `nodes` lists the nodes its containers run on and whether
/// each can record. Records are OpenTelemetry log records of the
/// environment's project (`logs_path` opens them, filtered to this
/// environment).
#[utoipa::path(
    tag = "Environments",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/egress-recording",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "Recording status", body = EgressRecordingStatus),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_environment_egress_recording(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EnvironmentEgressAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let status = state.service.status(project_id, environment_id).await?;
    Ok(Json(status))
}

/// Turn recording of the environment's outbound connections on or off.
///
/// With recording on, containers of the environment's deployments are
/// started with the per-node egress proxy as their HTTP(S) proxy, and
/// every connection they make through it produces one OpenTelemetry log
/// record when it ends, allowed or refused: destination (`server.address`,
/// `server.port`; method and path without the query for plain HTTP),
/// outcome and refusal reason, bytes each way and duration. Never headers,
/// bodies or query strings. Off by default.
///
/// It applies to containers started after it is turned on: redeploy the
/// environment to start recording. Turning it off stops recording at once.
/// Only clients that honour the proxy variables are recorded, and only
/// public destinations go through the proxy: internal addresses and names
/// stay direct. While it is on, the environment's outbound HTTP(S) depends
/// on the node's egress proxy being up. Turning it on fails with 422 when
/// no node the environment runs on can record, naming the environment, the
/// node and what is missing.
#[utoipa::path(
    tag = "Environments",
    put,
    path = "/projects/{project_id}/environments/{environment_id}/egress-recording",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SetEgressRecordingBody,
    responses(
        (status = 200, description = "Recording updated", body = EgressRecordingStatus),
        (status = 400, description = "Invalid body", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 422, description = "Recording cannot work for this environment: no node it runs on can record", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "OpenTelemetry storage is not enabled", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_environment_egress_recording(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EnvironmentEgressAppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(body): Json<SetEgressRecordingBody>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let (previous, status) = state
        .service
        .set_enabled(project_id, environment_id, body.enabled)
        .await?;

    let user_id = auth.user_id();
    let event = EgressRecordingUpdatedAudit::new(
        temps_core::AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        previous,
        &status,
    );
    if let Err(e) = state.audit_service.create_audit_log(&event).await {
        tracing::error!(
            user_id,
            project_id,
            environment_id,
            "environment egress recording: failed to write audit log: {e}"
        );
    }
    Ok(Json(status))
}

/// The environment's recorded outbound connections, newest first.
///
/// Each record is one connection (or identical refusals collapsed into
/// one, see `count`): destination host and port, method and path for plain
/// HTTP (never the query string), outcome and refusal reason, bytes each
/// way and duration, and the node whose proxy observed it. Records exist
/// only while recording is on. These are the same OpenTelemetry log
/// records the project's logs show, read by environment.
#[utoipa::path(
    tag = "Environments",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/egress-records",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        EgressActivityParams
    ),
    responses(
        (status = 200, description = "A page of recorded connections", body = EgressActivityPage),
        (status = 400, description = "Invalid query", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Environment not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "OpenTelemetry storage is not enabled or cannot be read", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_environment_egress_records(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EnvironmentEgressAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(params): Query<EgressActivityParams>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let page = state
        .service
        .activity(
            project_id,
            environment_id,
            params.page,
            params.per_page,
            params.outcome,
        )
        .await?;
    Ok(Json(page))
}

pub fn routes() -> Router<Arc<EnvironmentEgressAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/environments/{environment_id}/egress-recording",
            get(get_environment_egress_recording).put(set_environment_egress_recording),
        )
        .route(
            "/projects/{project_id}/environments/{environment_id}/egress-records",
            get(list_environment_egress_records),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{
        fixture, Fixture, ELSEWHERE, OTHER_PROJECT, PRODUCTION, PROJECT,
    };
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};
    use temps_entities::users;
    use tower::ServiceExt;

    fn member() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 6101,
            name: "Member".to_string(),
            email: "member@example.com".to_string(),
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

    #[derive(Default)]
    struct RecordingAudit(Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.0
                .lock()
                .unwrap()
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    fn app(f: &Fixture, audit: Arc<RecordingAudit>) -> Router {
        routes().with_state(Arc::new(EnvironmentEgressAppState {
            service: f.wired().environments,
            audit_service: audit,
            project_access_checker: None,
        }))
    }

    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
        auth: Option<AuthContext>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_default())
            .unwrap();
        request
            .extensions_mut()
            .insert(temps_core::RequestMetadata {
                ip_address: "198.51.100.20".to_string(),
                user_agent: "environment-egress-test".to_string(),
                headers: HeaderMap::new(),
                visitor_id_cookie: None,
                session_id_cookie: None,
                base_url: "http://localhost".to_string(),
                scheme: "http".to_string(),
                host: "localhost".to_string(),
                is_secure: false,
            });
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn switch(project: i32, environment: i32) -> String {
        format!("/projects/{project}/environments/{environment}/egress-recording")
    }

    fn records(project: i32, environment: i32, query: &str) -> String {
        format!("/projects/{project}/environments/{environment}/egress-records{query}")
    }

    #[tokio::test]
    async fn the_switch_needs_a_session_and_write_permission_and_is_audited() {
        let Some(f) = fixture().await else { return };
        let audit = Arc::new(RecordingAudit::default());
        let app = app(&f, audit.clone());
        let on = Some(r#"{"enabled":true}"#);
        let uri = switch(PROJECT, PRODUCTION);

        let (status, _) = send(app.clone(), "GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(app.clone(), "PUT", &uri, on, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let ingest_only = AuthContext::new_session(member(), Role::MetricsIngest);
        let (status, body) = send(app.clone(), "GET", &uri, None, Some(ingest_only)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["required_permission"], "environments:read", "{body}");

        let reader = AuthContext::new_session(member(), Role::Reader);
        let (status, body) = send(app.clone(), "PUT", &uri, on, Some(reader.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["required_permission"], "environments:write", "{body}");
        let (status, body) = send(app.clone(), "GET", &uri, None, Some(reader)).await;
        assert_eq!(status, StatusCode::OK, "readers may look: {body}");
        assert_eq!(body["workload_kind"], "deployment");
        assert_eq!(body["workload_id"], PRODUCTION.to_string());
        assert_eq!(body["enabled"], false);
        assert_eq!(body["nodes"][0]["node_name"], "control-plane");
        assert!(audit.0.lock().unwrap().is_empty());

        let user = AuthContext::new_session(member(), Role::User);
        let (status, body) = send(app.clone(), "PUT", &uri, on, Some(user.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["configured"], true);
        let entries = audit.0.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "DEPLOYMENT_EGRESS_RECORDING_UPDATED");
        let logged: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        assert_eq!(logged["workload_id"], PRODUCTION.to_string());
        assert_eq!(logged["project_id"], PROJECT);
        assert_eq!(
            (logged["from"].clone(), logged["to"].clone()),
            (false.into(), true.into())
        );
        assert_eq!(logged["context"]["ip_address"], "198.51.100.20");

        let (status, _) = send(app, "PUT", &uri, Some(r#"{"enable":true}"#), Some(user)).await;
        assert!(
            status.is_client_error(),
            "unknown fields are refused: {status}"
        );
    }

    #[tokio::test]
    async fn a_node_that_cannot_record_is_a_422_and_another_projects_environment_a_404() {
        let Some(f) = fixture().await else { return };
        let audit = Arc::new(RecordingAudit::default());
        let app = app(&f, audit.clone());
        let user = AuthContext::new_session(member(), Role::User);
        let on = Some(r#"{"enabled":true}"#);

        let (status, body) = send(
            app.clone(),
            "PUT",
            &switch(OTHER_PROJECT, ELSEWHERE),
            on,
            Some(user.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(
            body["type"],
            "https://temps.sh/probs/egress-recording-unavailable"
        );
        assert_eq!(body["workload"], "environment");
        assert_eq!(body["workload_id"], ELSEWHERE.to_string());
        assert_eq!(body["node_name"], "worker-mac");
        assert_eq!(body["reason_code"], "unsupported_platform");

        // The right environment under the wrong project is not found.
        let (status, body) = send(
            app.clone(),
            "PUT",
            &switch(OTHER_PROJECT, PRODUCTION),
            on,
            Some(user.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["title"], "Environment Not Found");
        assert!(
            audit.0.lock().unwrap().is_empty(),
            "a refused change is not audited"
        );

        let (status, body) = send(
            app.clone(),
            "GET",
            &records(PROJECT, PRODUCTION, "?page=1&per_page=5&outcome=refused"),
            None,
            Some(AuthContext::new_session(member(), Role::Reader)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["workload_id"], PRODUCTION.to_string());
        assert_eq!(body["per_page"], 5);
        assert_eq!(body["records"], serde_json::json!([]));

        let (status, _) = send(
            app.clone(),
            "GET",
            &records(PROJECT, PRODUCTION, ""),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, body) = send(
            app.clone(),
            "GET",
            &records(PROJECT, PRODUCTION, ""),
            None,
            Some(AuthContext::new_session(member(), Role::MetricsIngest)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, _) = send(
            app,
            "GET",
            &records(PROJECT, PRODUCTION, "?outcome=sideways"),
            None,
            Some(user),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
