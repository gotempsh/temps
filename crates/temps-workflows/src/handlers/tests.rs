// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflows API through its router: authentication, permissions,
//! Problem Details, masking, `no-store`, and audit records.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::{json, Value};
use temps_authz::context::AuthContext;
use temps_authz::permissions::Role;
use temps_core::{
    AuditLogger, AuditOperation, AuditResult, EncryptionService, ExecutorEndpointSet,
    ExecutorEndpoints, ExecutorEndpointsError, NoopProjectMetricsSink, RequestMetadata,
};
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use temps_entities::users;
use tower::ServiceExt;

use super::*;
use crate::engine::tests::{harness, Harness, Reply, Script};
use crate::metrics::WorkflowMetrics;
use crate::payload::PayloadCodec;
use crate::protocol::Outcome;
use crate::service::WorkflowService;
use crate::store::PgWorkflowStore;
use crate::token::InvocationSigner;

#[derive(Default)]
struct RecordingAudit(Mutex<Vec<(String, String)>>);

#[async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> AuditResult<()> {
        self.0
            .lock()
            .unwrap()
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

impl RecordingAudit {
    fn operations(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(op, _)| op.clone())
            .collect()
    }
}

struct NoEndpoints;

#[async_trait]
impl ExecutorEndpoints for NoEndpoints {
    async fn endpoints(
        &self,
        _project_id: i32,
        _environment_id: i32,
        _service: Option<&str>,
    ) -> Result<ExecutorEndpointSet, ExecutorEndpointsError> {
        Ok(ExecutorEndpointSet::unavailable(None, "No deployment"))
    }
}

fn user(id: i32) -> users::Model {
    let now = chrono::Utc::now();
    users::Model {
        id,
        name: "Test User".to_string(),
        email: "test@example.com".to_string(),
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

fn metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "test".to_string(),
        headers: HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

fn state_from(
    service: Arc<WorkflowService>,
    engine: Arc<crate::engine::WorkflowEngine>,
    audit: Arc<RecordingAudit>,
) -> Arc<WorkflowsAppState> {
    let signer =
        InvocationSigner::new(&EncryptionService::new("12345678901234567890123456789012").unwrap());
    Arc::new(WorkflowsAppState {
        service,
        engine,
        invocation_key: crate::protocol::InvocationKeyResponse {
            algorithm: "ed25519".into(),
            public_key: signer.public_key(),
            key_id: signer.key_id(),
        },
        audit_service: audit,
        project_access_checker: None,
    })
}

/// A router over a mock database: enough for everything the guards reject.
fn offline_app() -> Router {
    let encryption = Arc::new(EncryptionService::new("12345678901234567890123456789012").unwrap());
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let store = Arc::new(PgWorkflowStore::new(db));
    let codec = Arc::new(PayloadCodec::new(encryption.clone(), None));
    let metrics = Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink)));
    let engine = Arc::new(crate::engine::WorkflowEngine::new(
        store.clone(),
        codec.clone(),
        Arc::new(InvocationSigner::new(&encryption)),
        Arc::new(NoEndpoints),
        crate::executor::ExecutorClient::new().unwrap(),
        metrics.clone(),
    ));
    let service = Arc::new(WorkflowService::new(store, engine.clone(), codec, metrics));
    router(state_from(
        service,
        engine,
        Arc::new(RecordingAudit::default()),
    ))
}

fn router(state: Arc<WorkflowsAppState>) -> Router {
    configure_routes()
        .merge(configure_public_routes())
        .with_state(state)
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    auth: Option<AuthContext>,
    bearer: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    request.extensions_mut().insert(metadata());
    if let Some(auth) = auth {
        request.extensions_mut().insert(auth);
    }
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, headers, json)
}

const BASE: &str = "/projects/1/environments/2/workflows";
const RUN: &str = "00000000-0000-0000-0000-000000000001";

fn management_routes() -> Vec<(&'static str, String, Option<Value>)> {
    vec![
        ("GET", BASE.to_string(), None),
        ("GET", format!("{BASE}/runs"), None),
        (
            "POST",
            format!("{BASE}/runs"),
            Some(json!({"workflow": "orders"})),
        ),
        ("GET", format!("{BASE}/runs/{RUN}"), None),
        ("DELETE", format!("{BASE}/runs/{RUN}"), None),
        ("GET", format!("{BASE}/runs/{RUN}/steps"), None),
        (
            "POST",
            format!("{BASE}/runs/{RUN}/reveal"),
            Some(json!({"target": "input"})),
        ),
        ("POST", format!("{BASE}/runs/{RUN}/cancel"), None),
        ("POST", format!("{BASE}/runs/{RUN}/retry"), Some(json!({}))),
        ("POST", format!("{BASE}/runs/{RUN}/resume"), None),
        (
            "POST",
            format!("{BASE}/events"),
            Some(json!({"name": "paid"})),
        ),
        ("GET", format!("{BASE}/settings"), None),
        (
            "PUT",
            format!("{BASE}/settings"),
            Some(serde_json::to_value(crate::types::WorkflowSettings::default()).unwrap()),
        ),
        ("GET", format!("{BASE}/credential"), None),
        ("POST", format!("{BASE}/credential"), None),
        ("DELETE", format!("{BASE}/credential"), None),
    ]
}

#[tokio::test]
async fn every_management_route_requires_authentication() {
    for (method, uri, body) in management_routes() {
        let (status, _, _) = send(offline_app(), method, &uri, body, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn deployment_tokens_cannot_use_the_management_api() {
    for (method, uri, body) in management_routes() {
        let token = AuthContext::new_deployment_token(
            1,
            Some(2),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        );
        let (status, _, _) = send(offline_app(), method, &uri, body, Some(token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[tokio::test]
async fn roles_get_exactly_the_workflow_permissions_of_the_adr() {
    let reader = || AuthContext::new_session(user(1), Role::Reader);
    let member = || AuthContext::new_session(user(1), Role::User);
    // A reader can only read.
    for (method, uri, body) in management_routes() {
        if method == "GET" {
            continue;
        }
        let (status, _, problem) =
            send(offline_app(), method, &uri, body, Some(reader()), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "reader {method} {uri}");
        assert!(problem["detail"].as_str().unwrap().contains("permission"));
    }
    // A member operates runs but cannot reveal payloads, delete runs or
    // change settings and credentials.
    for (method, uri, body) in [
        (
            "POST",
            format!("{BASE}/runs/{RUN}/reveal"),
            Some(json!({"target": "input"})),
        ),
        ("DELETE", format!("{BASE}/runs/{RUN}"), None),
        (
            "PUT",
            format!("{BASE}/settings"),
            Some(serde_json::to_value(crate::types::WorkflowSettings::default()).unwrap()),
        ),
        ("POST", format!("{BASE}/credential"), None),
        ("DELETE", format!("{BASE}/credential"), None),
    ] {
        let (status, _, _) = send(offline_app(), method, &uri, body, Some(member()), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "member {method} {uri}");
    }
}

#[tokio::test]
async fn executor_and_app_routes_reject_missing_or_forged_tokens_as_problems() {
    for (method, uri, body) in [
        ("GET", format!("/workflow-runs/{RUN}/journal"), None),
        ("POST", format!("/workflow-runs/{RUN}/heartbeat"), None),
        (
            "POST",
            format!("/workflow-runs/{RUN}/outcome"),
            Some(json!({"outcome": {"type": "completed"}})),
        ),
        (
            "POST",
            "/workflows/v1/runs".to_string(),
            Some(json!({"workflow": "orders"})),
        ),
        ("GET", format!("/workflows/v1/runs/{RUN}"), None),
        (
            "POST",
            "/workflows/v1/events".to_string(),
            Some(json!({"name": "paid"})),
        ),
    ] {
        let (status, headers, problem) =
            send(offline_app(), method, &uri, body.clone(), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(headers["content-type"], "application/problem+json");
        assert_eq!(problem["title"], "Missing Bearer Token");

        let forged = if uri.starts_with("/workflows/v1") {
            // Malformed: rejected before any lookup (an unknown `twf_` token
            // is covered by the revocation test against a real database).
            "tk_not_a_workflow_credential"
        } else {
            "twi1.forged.token"
        };
        let (status, _, problem) =
            send(offline_app(), method, &uri, body, None, Some(forged)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri}: {problem}"
        );
    }
}

#[tokio::test]
async fn the_invocation_key_is_public() {
    let (status, _, body) = send(
        offline_app(),
        "GET",
        "/workflows/v1/invocation-key",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["algorithm"], "ed25519");
    assert_eq!(body["key_id"].as_str().unwrap().len(), 16);
}

fn completing() -> Script {
    Arc::new(|_, _, _| {
        Box::pin(async {
            Reply::Outcome(Outcome::Completed {
                output: Some(json!({ "receipt": "r-1" })),
            })
        })
    })
}

async fn live(test: &str) -> Option<(Harness, Arc<RecordingAudit>, Router)> {
    let h = harness(test, &["orders"], completing(), true).await?;
    let audit = Arc::new(RecordingAudit::default());
    let service = Arc::new(WorkflowService::new(
        h.store.clone(),
        h.engine.clone(),
        h.codec.clone(),
        Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink))),
    ));
    let app = router(state_from(service, h.engine.clone(), audit.clone()));
    Some((h, audit, app))
}

#[tokio::test]
async fn an_operator_starts_inspects_and_reveals_a_run_with_every_write_audited() {
    let Some((h, audit, app)) = live("handler run test").await else {
        return;
    };
    let admin = || AuthContext::new_session(user(h.env.user_id), Role::Admin);
    let base = format!(
        "/projects/{}/environments/{}/workflows",
        h.env.project_id, h.env.environment_id
    );
    let secret = "card-4242-secret";

    let (status, _, started) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs"),
        Some(json!({"workflow": "orders", "input": {"card": secret}, "idempotency_key": "o-1"})),
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    assert_eq!(started["created"], true);
    let run_id = started["run"]["id"].as_str().unwrap().to_string();
    assert!(
        !started.to_string().contains(secret),
        "input leaked: {started}"
    );

    // The same key returns the same run, 200, with no second audit record.
    let (status, _, again) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs"),
        Some(json!({"workflow": "orders", "idempotency_key": "o-1"})),
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["run"]["id"], run_id.as_str());

    let (status, _, list) = send(
        app.clone(),
        "GET",
        &format!("{base}/runs"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1);
    assert_eq!(list["page_size"], 20);
    let (status, _, run) = send(
        app.clone(),
        "GET",
        &format!("{base}/runs/{run_id}"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["has_input"], true);
    assert!(!run.to_string().contains(secret));

    let (status, headers, revealed) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs/{run_id}/reveal"),
        Some(json!({"target": "input"})),
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(revealed["value"]["card"], secret);

    // A missing step is a Problem naming it.
    let (status, _, problem) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs/{run_id}/reveal"),
        Some(json!({"target": "step_result", "sequence": 4})),
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["title"], "Workflow Step Not Found");
    assert!(problem["detail"].as_str().unwrap().contains(&run_id));

    let (status, _, cancelled) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs/{run_id}/cancel"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    let (status, _, problem) = send(
        app.clone(),
        "POST",
        &format!("{base}/runs/{run_id}/cancel"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["title"], "Invalid Workflow Run State");

    let (status, _, _) = send(
        app.clone(),
        "DELETE",
        &format!("{base}/runs/{run_id}"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let operations = audit.operations();
    assert_eq!(
        operations,
        [
            "WORKFLOW_RUN_STARTED",
            "WORKFLOW_PAYLOAD_REVEALED",
            "WORKFLOW_RUN_CHANGED",
            "WORKFLOW_RUN_DELETED"
        ]
    );
    for (_, record) in audit.0.lock().unwrap().iter() {
        assert!(
            !record.contains(secret),
            "audit record holds the payload: {record}"
        );
    }
}

#[tokio::test]
async fn an_app_uses_its_credential_to_start_read_and_signal_runs() {
    let Some((h, audit, app)) = live("handler app test").await else {
        return;
    };
    let admin = || AuthContext::new_session(user(h.env.user_id), Role::Admin);
    let base = format!(
        "/projects/{}/environments/{}/workflows",
        h.env.project_id, h.env.environment_id
    );
    let (status, headers, issued) = send(
        app.clone(),
        "POST",
        &format!("{base}/credential"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers["cache-control"], "no-store");
    let token = issued["token"].as_str().unwrap().to_string();
    // The status endpoint never shows it again.
    let (_, _, status_body) = send(
        app.clone(),
        "GET",
        &format!("{base}/credential"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status_body["configured"], true);
    assert!(!status_body.to_string().contains(&token));

    let (status, _, started) = send(
        app.clone(),
        "POST",
        "/workflows/v1/runs",
        Some(json!({"workflow": "orders", "input": 1})),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let run_id = started["run"]["id"].as_str().unwrap().to_string();

    for claim in h.engine.claim(10).await.unwrap() {
        h.engine.invoke(claim).await;
    }
    let (status, _, run) = send(
        app.clone(),
        "GET",
        &format!("/workflows/v1/runs/{run_id}"),
        None,
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["status"], "completed");
    assert_eq!(run["output"]["receipt"], "r-1");

    let (status, _, sent) = send(
        app.clone(),
        "POST",
        "/workflows/v1/events",
        Some(json!({"name": "paid", "key": "o-1"})),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent["delivered"], 0);

    // Revoked: the app is locked out.
    let (status, _, _) = send(
        app.clone(),
        "DELETE",
        &format!("{base}/credential"),
        None,
        Some(admin()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, problem) = send(
        app.clone(),
        "GET",
        &format!("/workflows/v1/runs/{run_id}"),
        None,
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["title"], "Invalid Workflow Credential");

    assert_eq!(
        audit.operations(),
        [
            "WORKFLOW_CREDENTIAL_ISSUED",
            "WORKFLOW_RUN_STARTED",
            "WORKFLOW_EVENT_SENT",
            "WORKFLOW_CREDENTIAL_REVOKED"
        ]
    );
    for (_, record) in audit.0.lock().unwrap().iter() {
        assert!(!record.contains(&token), "audit record holds the token");
    }
}

#[tokio::test]
async fn the_overview_shows_an_onboarding_state_when_nothing_can_run() {
    let Some(h) = harness("handler overview test", &["orders"], completing(), false).await else {
        return;
    };
    let service = Arc::new(WorkflowService::new(
        h.store.clone(),
        h.engine.clone(),
        h.codec.clone(),
        Arc::new(WorkflowMetrics::new(Arc::new(NoopProjectMetricsSink))),
    ));
    let app = router(state_from(
        service,
        h.engine.clone(),
        Arc::new(RecordingAudit::default()),
    ));
    let (status, _, overview) = send(
        app,
        "GET",
        &format!(
            "/projects/{}/environments/{}/workflows",
            h.env.project_id, h.env.environment_id
        ),
        None,
        Some(AuthContext::new_session(user(h.env.user_id), Role::Reader)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["executor"]["configured"], false);
    assert!(overview["executor"]["reason"]
        .as_str()
        .unwrap()
        .contains("No deployment"));
    assert_eq!(
        overview["executor"]["setup_path"],
        crate::service::EXECUTOR_SETUP_PATH
    );
    assert_eq!(overview["credential"]["configured"], false);
}
