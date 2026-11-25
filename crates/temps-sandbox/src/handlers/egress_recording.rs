// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a sandbox's outbound connections: the owner's switch and its
//! capability, and the node endpoint that delivers the records.
//!
//! - `GET /v1/sandboxes/{id}/egress-recording`: whether it records, and
//!   whether it can (and if not, exactly what is missing).
//! - `PUT /v1/sandboxes/{id}/egress-recording` `{"enabled": true}`: turn it
//!   on or off. Audit-logged.
//! - `GET /v1/sandboxes/{id}/egress-records`: the sandbox's recorded
//!   connections, newest first, paginated.
//! - `POST /internal/nodes/{node_id}/egress-records`: a worker's batch,
//!   authenticated with the node token, mounted outside the plugin router
//!   next to the network-policy report.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::RequireAuth;
use temps_core::problemdetails::{self, Problem};
use temps_workload_netpol::EgressRecordBatch;
use utoipa::ToSchema;

use crate::handlers::sandboxes::{require_sandbox_read, require_sandbox_write};
use crate::services::egress_recording::activity::{EgressActivityPage, EgressOutcomeFilter};
use crate::services::egress_recording::ingest::EgressIngestOutcome;
use crate::services::egress_recording::{
    EgressRecordingError, EgressRecordingService, EgressRecordingStatus,
};

/// The largest batch body a node may send: 512 bounded records fit well
/// inside it.
pub const MAX_BATCH_BODY: usize = 1024 * 1024;

pub struct EgressRecordingAppState {
    pub service: Arc<EgressRecordingService>,
    /// Audit failures are logged and never fail the request.
    pub audit_service: Option<Arc<dyn temps_core::AuditLogger>>,
}

impl From<EgressRecordingError> for Problem {
    fn from(error: EgressRecordingError) -> Self {
        let detail = error.to_string();
        match &error {
            EgressRecordingError::SandboxNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Sandbox Not Found")
                    .with_detail(detail)
            }
            EgressRecordingError::NoProject { sandbox_id } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/sandbox-egress-recording-needs-project")
                    .with_title("Sandbox Has No Project To Record Into")
                    .with_detail(detail)
                    .with_value("sandbox_id", sandbox_id.clone())
            }
            EgressRecordingError::Unavailable {
                sandbox_id,
                node_id,
                node_name,
                code,
                ..
            } => problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                .with_type("https://temps.sh/probs/sandbox-egress-recording-unavailable")
                .with_title("Egress Recording Unavailable On This Node")
                .with_detail(detail)
                .with_value("sandbox_id", sandbox_id.clone())
                .with_value("node_id", *node_id)
                .with_value("node_name", node_name.clone())
                .with_value("reason_code", code.to_string()),
            EgressRecordingError::OtelUnavailable { sandbox_id } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/sandbox-egress-recording-unavailable")
                    .with_title("OpenTelemetry Storage Unavailable")
                    .with_detail(detail)
                    .with_value("sandbox_id", sandbox_id.clone())
                    .with_value("reason_code", "otel_unavailable")
            }
            EgressRecordingError::BatchTooLarge { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Egress Record Batch Too Large")
                    .with_detail(detail)
            }
            EgressRecordingError::InvalidRecord { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Egress Record")
                    .with_detail(detail)
            }
            EgressRecordingError::NotPlacedOnNode { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Sandbox Not Placed On This Node")
                    .with_detail(detail)
            }
            EgressRecordingError::UnknownSandbox { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Unknown Sandbox")
                    .with_detail(detail)
            }
            EgressRecordingError::IngestUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("OpenTelemetry Storage Unavailable")
                    .with_detail(detail)
            }
            EgressRecordingError::Store { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Egress Records Not Stored")
                    .with_detail(detail)
            }
            EgressRecordingError::Query { sandbox_id, .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Recorded Connections Unreadable")
                    .with_detail(detail)
                    .with_value("sandbox_id", sandbox_id.clone())
            }
            EgressRecordingError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(detail)
            }
        }
    }
}

/// Turn recording of the sandbox's outbound connections on or off.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetEgressRecordingBody {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SandboxEgressRecordingUpdatedAudit {
    context: temps_core::AuditContext,
    sandbox_id: String,
    node_id: Option<i32>,
    from: bool,
    to: bool,
}

impl temps_core::AuditOperation for SandboxEgressRecordingUpdatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_EGRESS_RECORDING_UPDATED".to_string()
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
        serde_json::to_string(self).map_err(|source| {
            temps_core::AuditError::serialize(
                format!(
                    "SandboxEgressRecordingUpdatedAudit for sandbox {}",
                    self.sandbox_id
                ),
                source,
            )
        })
    }
}

/// Whether the sandbox records its outbound connections, and whether it can.
///
/// `configured: false` says recording cannot work for this sandbox now, with
/// `reason_code`, `reason` and, when the console can fix it, `setup_path`.
/// Records are OpenTelemetry log records of the sandbox's project
/// (`logs_path` opens them, filtered to this sandbox).
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
/// the per-node egress proxy produces one OpenTelemetry log record of the
/// sandbox's project when it ends, allowed or refused: destination
/// (`server.address`, `server.port`; method and path without the query for
/// plain HTTP), outcome and refusal reason, bytes each way and duration.
/// Never headers, bodies or query strings. Off by default. Turning it on
/// fails with 422 when the sandbox has no project or its node cannot
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
        (status = 422, description = "Recording cannot work for this sandbox: no project, or its node cannot record", body = temps_core::problemdetails::ProblemDetails),
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
        let event = SandboxEgressRecordingUpdatedAudit {
            context: temps_core::AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            sandbox_id: id.clone(),
            node_id: status.node_id,
            from: previous,
            to: status.enabled,
        };
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

/// Which page of recorded connections, and which outcomes.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EgressActivityParams {
    /// Page number, from 1. Default 1.
    pub page: Option<u64>,
    /// Records per page. Default 20, at most 100.
    pub per_page: Option<u64>,
    /// Only connections that ended this way.
    pub outcome: Option<EgressOutcomeFilter>,
}

/// The sandbox's recorded outbound connections, newest first.
///
/// Each record is one connection (or identical refusals collapsed into
/// one, see `count`): destination host and port, method and path for plain
/// HTTP (never the query string), outcome and refusal reason, bytes each
/// way and duration. Records exist only while recording is on; a sandbox
/// without a project has none. These are the same OpenTelemetry log
/// records the project's logs show, read by sandbox.
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

pub struct EgressRecordingNodeState {
    pub service: Arc<EgressRecordingService>,
}

/// The worker endpoint: `POST /internal/nodes/{node_id}/egress-records`.
pub fn node_routes(state: Arc<EgressRecordingNodeState>) -> Router {
    Router::new()
        .route(
            "/internal/nodes/{node_id}/egress-records",
            post(ingest_node_records),
        )
        .layer(DefaultBodyLimit::max(MAX_BATCH_BODY))
        .with_state(state)
}

async fn ingest_node_records(
    State(state): State<Arc<EgressRecordingNodeState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EgressIngestOutcome>, Problem> {
    // Authenticated before the body is parsed.
    state
        .service
        .authenticate_node(&headers, node_id)
        .await
        .map_err(|(status, detail)| {
            problemdetails::new(status)
                .with_title("Node Authentication Failed")
                .with_detail(detail)
        })?;
    let batch: EgressRecordBatch = serde_json::from_slice(&body).map_err(|e| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Egress Record Batch")
            .with_detail(format!(
                "egress record batch from node {node_id} is not valid JSON: {e}"
            ))
    })?;
    let outcome = state.service.ingest(node_id, batch).await?;
    Ok(Json(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::egress_recording::tests::{
        fixture, record, Fixture, NODE_TOKEN, ON_OTHER_WORKER, ON_WORKER, OWNER,
    };
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Mutex;
    use temps_auth::{AuthContext, Role};
    use temps_entities::users;
    use temps_workload_netpol::ConnectionOutcome;
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
        let (service, _) = f.service();
        routes().with_state(Arc::new(EgressRecordingAppState {
            service,
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
            service: f.service_with_otel(),
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
        assert_eq!(body["sandbox_id"], ON_WORKER.1);
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
        assert_eq!(body["node_name"], "worker-a");
        let entries = audit.0.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "SANDBOX_EGRESS_RECORDING_UPDATED");
        let logged: serde_json::Value = serde_json::from_str(&entries[0].1).unwrap();
        assert_eq!(logged["sandbox_id"], ON_WORKER.1);
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
            "https://temps.sh/probs/sandbox-egress-recording-unavailable"
        );
        assert_eq!(body["sandbox_id"], ON_OTHER_WORKER.1);
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
        let (service, store) = f.service();
        service.set_enabled(ON_WORKER.1, OWNER, true).await.unwrap();
        let app = node_routes(Arc::new(EgressRecordingNodeState { service }));
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
        assert_eq!(body["title"], "Sandbox Not Placed On This Node");

        let (status, body) = post_batch(app, f.worker_a, Some(NODE_TOKEN), &own).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stored"], 1);
        assert_eq!(store.0.lock().unwrap().len(), 1);
    }
}
