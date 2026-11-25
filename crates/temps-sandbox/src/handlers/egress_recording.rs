// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a sandbox's outbound connections: the owner's switch, its
//! capability and its recorded connections.
//!
//! - `GET /v1/sandboxes/{id}/egress-recording`: whether it records, and
//!   whether it can (and if not, exactly what is missing).
//! - `PUT /v1/sandboxes/{id}/egress-recording` `{"enabled": true}`: turn it
//!   on or off. Audit-logged.
//! - `GET /v1/sandboxes/{id}/egress-records`: the sandbox's recorded
//!   connections, newest first, paginated.
//!
//! These are the sandbox's URLs and the sandbox's authorization. Everything
//! behind them (the flow, the request and response types, the audit entry,
//! the errors) is `temps-workload-egress`, shared with every other
//! workload kind, as is the node endpoint that delivers the records.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json, Router};
use temps_auth::RequireAuth;
use temps_core::problemdetails::Problem;
use temps_workload_egress::handlers::{
    EgressActivityParams, EgressRecordingUpdatedAudit, SetEgressRecordingBody,
};
use temps_workload_egress::{EgressActivityPage, EgressRecordingStatus};

use crate::handlers::sandboxes::{require_sandbox_read, require_sandbox_write};
use crate::services::egress_recording::SandboxEgress;

pub struct EgressRecordingAppState {
    pub service: Arc<SandboxEgress>,
    /// Audit failures are logged and never fail the request.
    pub audit_service: Option<Arc<dyn temps_core::AuditLogger>>,
}

/// Whether the sandbox records its outbound connections, and whether it can.
///
/// `configured: false` says recording cannot work for this sandbox now, with
/// `reason_code`, `reason` and, when the console can fix it, `setup_path`.
/// Records are OpenTelemetry log records owned by the sandbox, and by its
/// project when it has one (`logs_path` then opens them in the project's
/// logs, filtered to this sandbox).
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/egress-recording",
    params(("id" = String, Path, description = "Sandbox id (`sbx_...`)")),
    responses(
        (status = 200, description = "Recording status", body = EgressRecordingStatus),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Sandbox not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_egress_recording(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EgressRecordingAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_read(&auth)?;
    let status = state.service.status(&id, auth.user_id()).await?;
    Ok(Json(status))
}

/// Turn recording of the sandbox's outbound connections on or off.
///
/// With recording on, every outbound connection the sandbox makes through
/// the per-node egress proxy produces one OpenTelemetry log record when it
/// ends, allowed or refused: destination
/// (`server.address`, `server.port`; method and path without the query for
/// plain HTTP), outcome and refusal reason, bytes each way and duration.
/// Never headers, bodies or query strings. Off by default. A sandbox needs
/// no project to record. Turning it on fails with 422 when its node cannot
/// record, naming the sandbox, the node and what is missing.
#[utoipa::path(
    tag = "Sandboxes",
    put,
    path = "/v1/sandboxes/{id}/egress-recording",
    params(("id" = String, Path, description = "Sandbox id (`sbx_...`)")),
    request_body = SetEgressRecordingBody,
    responses(
        (status = 200, description = "Recording updated", body = EgressRecordingStatus),
        (status = 400, description = "Invalid body", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Sandbox not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 422, description = "Recording cannot work for this sandbox: its node cannot record", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "OpenTelemetry storage is not enabled", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_egress_recording(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EgressRecordingAppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Path(id): Path<String>,
    Json(body): Json<SetEgressRecordingBody>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_write(&auth)?;
    let user_id = auth.user_id();
    let (previous, status) = state
        .service
        .set_enabled(&id, user_id, body.enabled)
        .await?;

    if let Some(audit) = &state.audit_service {
        let event = EgressRecordingUpdatedAudit::new(
            temps_core::AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            previous,
            &status,
        );
        if let Err(e) = audit.create_audit_log(&event).await {
            tracing::error!(
                user_id,
                sandbox_id = %id,
                "sandbox egress recording: failed to write audit log: {e}"
            );
        }
    }
    Ok(Json(status))
}

/// The sandbox's recorded outbound connections, newest first.
///
/// Each record is one connection (or identical refusals collapsed into
/// one, see `count`): destination host and port, method and path for plain
/// HTTP (never the query string), outcome and refusal reason, bytes each
/// way and duration. Records exist only while recording is on. These are
/// the same OpenTelemetry log records the project's logs show when the
/// sandbox has a project, read by sandbox.
#[utoipa::path(
    tag = "Sandboxes",
    get,
    path = "/v1/sandboxes/{id}/egress-records",
    params(
        ("id" = String, Path, description = "Sandbox id (`sbx_...`)"),
        EgressActivityParams
    ),
    responses(
        (status = 200, description = "A page of recorded connections", body = EgressActivityPage),
        (status = 400, description = "Invalid query", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Sandbox not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 503, description = "OpenTelemetry storage is not enabled or cannot be read", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_egress_records(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<EgressRecordingAppState>>,
    Path(id): Path<String>,
    Query(params): Query<EgressActivityParams>,
) -> Result<impl IntoResponse, Problem> {
    require_sandbox_read(&auth)?;
    let page = state
        .service
        .activity(
            &id,
            auth.user_id(),
            params.page,
            params.per_page,
            params.outcome,
        )
        .await?;
    Ok(Json(page))
}

pub fn routes() -> Router<Arc<EgressRecordingAppState>> {
    Router::new()
        .route(
            "/v1/sandboxes/{id}/egress-recording",
            get(get_egress_recording).put(set_egress_recording),
        )
        .route(
            "/v1/sandboxes/{id}/egress-records",
            get(list_egress_records),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{
        fixture, record, Fixture, NODE_TOKEN, ON_OTHER_WORKER, ON_WORKER, OWNER,
    };
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};
    use temps_entities::users;
    use temps_workload_egress::handlers::{node_routes, EgressRecordingNodeState};
    use temps_workload_netpol::{ConnectionOutcome, EgressRecordBatch};
    use tower::ServiceExt;

    fn owner() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: OWNER,
            name: "Owner".to_string(),
            email: "owner@example.com".to_string(),
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
        let (wired, _) = f.collecting();
        routes().with_state(Arc::new(EgressRecordingAppState {
            service: wired.sandboxes,
            audit_service: Some(audit),
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
                user_agent: "egress-recording-test".to_string(),
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
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn uri(id: &str) -> String {
        format!("/v1/sandboxes/{id}/egress-recording")
    }

    #[tokio::test]
    async fn recorded_connections_need_a_session_and_read_permission() {
        let Some(f) = fixture().await else { return };
        let app = routes().with_state(Arc::new(EgressRecordingAppState {
            service: f.with_otel().sandboxes,
            audit_service: None,
        }));
        let records = |query: &str| format!("/v1/sandboxes/{}/egress-records{query}", ON_WORKER.1);

        let (status, _) = send(app.clone(), "GET", &records(""), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let ingest_only = AuthContext::new_session(owner(), Role::MetricsIngest);
        let (status, body) = send(app.clone(), "GET", &records(""), None, Some(ingest_only)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["required_permission"], "sandboxes:read", "{body}");

        let reader = AuthContext::new_session(owner(), Role::Reader);
        let (status, body) = send(
            app.clone(),
            "GET",
            &records("?page=1&per_page=5&outcome=refused"),
            None,
            Some(reader.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["workload_kind"], "sandbox");
        assert_eq!(body["workload_id"], ON_WORKER.1);
        assert_eq!(body["per_page"], 5);
        assert_eq!(body["records"], serde_json::json!([]));
        assert_eq!(body["has_more"], false);

        let (status, _) = send(
            app.clone(),
            "GET",
            &records("?outcome=sideways"),
            None,
            Some(reader.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, body) = send(
            app,
            "GET",
            "/v1/sandboxes/sbx_0000000000000000/egress-records",
            None,
            Some(reader),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["title"], "Sandbox Not Found");
    }

    #[tokio::test]
    async fn the_switch_needs_a_session_and_write_permission_and_is_audited() {
        let Some(f) = fixture().await else { return };
        let audit = Arc::new(RecordingAudit::default());
        let app = app(&f, audit.clone());

        let (status, _) = send(app.clone(), "GET", &uri(ON_WORKER.1), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let on = Some(r#"{"enabled":true}"#);
        let (status, _) = send(app.clone(), "PUT", &uri(ON_WORKER.1), on, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        for role in [Role::Reader, Role::ApiReader] {
            let reader = AuthContext::new_session(owner(), role);
            let (status, body) = send(
                app.clone(),
                "PUT",
                &uri(ON_WORKER.1),
                on,
                Some(reader.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(
                body["required_permission"], "sandboxes:write",
                "a problem body names the permission: {body}"
            );
            let (status, body) =
                send(app.clone(), "GET", &uri(ON_WORKER.1), None, Some(reader)).await;
            assert_eq!(status, StatusCode::OK, "readers may look: {body}");
            assert_eq!(body["enabled"], false);
        }
        assert!(audit.0.lock().unwrap().is_empty());

        let user = AuthContext::new_session(owner(), Role::User);
        let (status, body) = send(app.clone(), "PUT", &uri(ON_WORKER.1), on, Some(user)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["configured"], true);
        assert_eq!(body["nodes"][0]["node_name"], "worker-a");
        let entries = audit.0.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "SANDBOX_EGRESS_RECORDING_UPDATED");
        let logged: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        assert_eq!(logged["workload_kind"], "sandbox");
        assert_eq!(logged["workload_id"], ON_WORKER.1);
        assert_eq!(
            (logged["from"].clone(), logged["to"].clone()),
            (false.into(), true.into())
        );
        assert_eq!(logged["context"]["ip_address"], "198.51.100.20");
    }

    #[tokio::test]
    async fn an_unavailable_node_is_a_422_naming_the_sandbox_and_the_node() {
        let Some(f) = fixture().await else { return };
        let audit = Arc::new(RecordingAudit::default());
        let user = AuthContext::new_session(owner(), Role::User);
        let (status, body) = send(
            app(&f, audit.clone()),
            "PUT",
            &uri(ON_OTHER_WORKER.1),
            Some(r#"{"enabled":true}"#),
            Some(user),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(
            body["type"],
            "https://temps.sh/probs/egress-recording-unavailable"
        );
        assert_eq!(body["workload"], "sandbox");
        assert_eq!(body["workload_id"], ON_OTHER_WORKER.1);
        assert_eq!(body["node_id"], f.worker_b);
        assert_eq!(body["node_name"], "worker-b");
        assert_eq!(body["reason_code"], "unsupported_platform");
        assert!(body["detail"].as_str().unwrap().contains("macos"));
        assert!(
            audit.0.lock().unwrap().is_empty(),
            "a refused change is not audited"
        );
    }

    #[tokio::test]
    async fn a_body_with_unknown_fields_and_another_users_sandbox_are_refused() {
        let Some(f) = fixture().await else { return };
        let user = AuthContext::new_session(owner(), Role::User);
        let app = app(&f, Arc::new(RecordingAudit::default()));
        let (status, _) = send(
            app.clone(),
            "PUT",
            &uri(ON_WORKER.1),
            Some(r#"{"enable":true}"#),
            Some(user.clone()),
        )
        .await;
        assert!(status.is_client_error(), "{status}");
        let mut stranger = owner();
        stranger.id = crate::services::egress_recording::tests::STRANGER;
        let (status, body) = send(
            app,
            "GET",
            &uri(ON_WORKER.1),
            None,
            Some(AuthContext::new_session(stranger, Role::User)),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["title"], "Sandbox Not Found");
    }

    async fn post_batch(
        app: Router,
        node_id: i32,
        token: Option<&str>,
        batch: &EgressRecordBatch,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("/internal/nodes/{node_id}/egress-records"))
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(Body::from(serde_json::to_vec(batch).unwrap()))
            .unwrap();
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

    #[tokio::test]
    async fn the_node_endpoint_takes_only_the_nodes_own_sandboxes_with_its_token() {
        let Some(f) = fixture().await else { return };
        let (wired, store) = f.collecting();
        wired
            .sandboxes
            .set_enabled(ON_WORKER.1, OWNER, true)
            .await
            .unwrap();
        let app = node_routes(Arc::new(EgressRecordingNodeState {
            service: wired.recording,
        }));
        let own = EgressRecordBatch {
            records: vec![record(
                ON_WORKER.0,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )],
            dropped: 0,
        };
        let (status, _) = post_batch(app.clone(), f.worker_a, None, &own).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = post_batch(app.clone(), f.worker_a, Some("wrong"), &own).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Worker A's token does not authenticate worker B.
        let (status, _) = post_batch(app.clone(), f.worker_b, Some(NODE_TOKEN), &own).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(store.0.lock().unwrap().is_empty());

        let foreign = EgressRecordBatch {
            records: vec![record(
                ON_OTHER_WORKER.0,
                "api.example.com",
                ConnectionOutcome::Allowed,
            )],
            dropped: 0,
        };
        let (status, body) = post_batch(app.clone(), f.worker_a, Some(NODE_TOKEN), &foreign).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["title"], "Workload Not Placed On This Node");

        let (status, body) = post_batch(app, f.worker_a, Some(NODE_TOKEN), &own).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stored"], 1);
        assert_eq!(store.0.lock().unwrap().len(), 1);
    }
}
