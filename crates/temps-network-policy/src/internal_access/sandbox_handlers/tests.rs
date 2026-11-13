// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who may list, create and delete sandbox grants, and see a sandbox's
//! internal addresses (ADR 105).

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{HeaderMap, Request};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use temps_authz::{AuthContext, Role};
use temps_core::AuditLogger;
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use temps_entities::users;
use tower::ServiceExt;

use temps_core::ai_tool_call::{AiToolCall, ConfirmedAiWrite};

use super::super::error::InternalAccessError;
use super::super::handlers::router;
use super::super::service::InternalAccessService;
use super::*;

const TARGET: i32 = 1;

type Row = BTreeMap<&'static str, sea_orm::Value>;

#[derive(Default)]
struct RecordingAudit {
    operations: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        if let Ok(mut operations) = self.operations.lock() {
            operations.push((operation.operation_type(), operation.serialize()?));
        }
        Ok(())
    }
}

struct Membership(HashSet<i32>);

#[async_trait::async_trait]
impl temps_core::ProjectAccessChecker for Membership {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.contains(&project_id))
    }
}

fn user() -> users::Model {
    let now = chrono::Utc::now();
    users::Model {
        id: 9,
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

fn state(
    db: MockDatabase,
    members_of: Option<&[i32]>,
) -> (Arc<InternalAccessAppState>, Arc<RecordingAudit>) {
    let audit = Arc::new(RecordingAudit::default());
    let state = Arc::new(InternalAccessAppState {
        service: Arc::new(InternalAccessService::new(Arc::new(db.into_connection()))),
        audit: audit.clone(),
        project_access_checker: members_of.map(|projects| {
            Arc::new(Membership(projects.iter().copied().collect()))
                as Arc<dyn temps_core::ProjectAccessChecker>
        }),
    });
    (state, audit)
}

fn untouched() -> MockDatabase {
    MockDatabase::new(DatabaseBackend::Postgres)
}

fn id_row(id: i32) -> Row {
    BTreeMap::from([("id", id.into())])
}

fn count_row(n: i64) -> Row {
    BTreeMap::from([("n", n.into())])
}

fn view_row(id: i32) -> Row {
    BTreeMap::from([
        ("id", id.into()),
        ("target_project_id", TARGET.into()),
        ("target_project_name", "Orders API".into()),
        ("target_project_slug", "orders-api".into()),
        ("target_environment_id", Option::<i32>::None.into()),
        ("target_environment_name", Option::<String>::None.into()),
        ("sandbox_id", "sbx_alpha".into()),
        ("sandbox_name", "temps-sandbox-alpha".into()),
        ("sandbox_status", "running".into()),
        (
            "description",
            Some("agent calls the api".to_string()).into(),
        ),
        ("created_by", Some(9).into()),
        ("created_at", chrono::Utc::now().into()),
    ])
}

fn create_succeeds() -> MockDatabase {
    let inserted = BTreeMap::from([
        ("id", 41.into()),
        ("target_project_id", TARGET.into()),
        ("target_environment_id", Option::<i32>::None.into()),
        ("sandbox_id", 5.into()),
        (
            "description",
            Some("agent calls the api".to_string()).into(),
        ),
        ("created_by", Some(9).into()),
        ("created_at", chrono::Utc::now().into()),
    ]);
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![id_row(TARGET)]])
        .append_query_results([vec![id_row(5)]])
        .append_query_results([vec![count_row(0)]])
        .append_query_results([vec![inserted]])
        .append_query_results([vec![view_row(41)]])
}

async fn create_status(auth: AuthContext, state: Arc<InternalAccessAppState>) -> StatusCode {
    match create_as(auth, state, None, None).await {
        Ok(status) => status,
        Err(problem) => problem.status_code,
    }
}

/// Create a sandbox grant the way the assistant's replayed request reaches
/// the handler: `ai_call` marks it as the assistant's, `confirmed` as the
/// owner's confirmation of that exact pending action in the console.
async fn create_as(
    auth: AuthContext,
    state: Arc<InternalAccessAppState>,
    ai_call: Option<AiToolCall>,
    confirmed: Option<ConfirmedAiWrite>,
) -> Result<StatusCode, Problem> {
    create_internal_access_sandbox_grant(
        RequireAuth(auth),
        State(state),
        Extension(metadata()),
        ai_call.map(Extension),
        confirmed.map(Extension),
        Path(TARGET),
        Json(CreateInternalAccessSandboxGrantRequest {
            target_environment_id: None,
            sandbox_id: "sbx_alpha".to_string(),
            description: Some("agent calls the api".to_string()),
        }),
    )
    .await
    .map(|response| response.into_response().status())
}

async fn delete_status(auth: AuthContext, state: Arc<InternalAccessAppState>) -> StatusCode {
    match delete_internal_access_sandbox_grant(
        RequireAuth(auth),
        State(state),
        Extension(metadata()),
        Path((TARGET, 41)),
    )
    .await
    {
        Ok(response) => response.into_response().status(),
        Err(problem) => problem.status_code,
    }
}

async fn addresses(
    auth: AuthContext,
    state: Arc<InternalAccessAppState>,
) -> Result<axum::response::Response, Problem> {
    list_sandbox_internal_addresses(
        RequireAuth(auth),
        State(state),
        Path("sbx_alpha".to_string()),
        Query(SandboxInternalAddressQuery {
            page: None,
            page_size: None,
        }),
    )
    .await
    .map(IntoResponse::into_response)
}

fn deployment_token() -> AuthContext {
    AuthContext::new_deployment_token(
        TARGET,
        None,
        None,
        1,
        "app".to_string(),
        vec![DeploymentTokenPermission::FullAccess],
    )
}

#[tokio::test]
async fn every_endpoint_requires_authentication() {
    let (state, _) = state(untouched(), None);
    for (method, uri, body) in [
        (
            "GET",
            "/projects/1/internal-access/sandboxes",
            Body::empty(),
        ),
        (
            "POST",
            "/projects/1/internal-access/sandboxes",
            Body::from(r#"{"sandbox_id":"sbx_alpha"}"#),
        ),
        (
            "DELETE",
            "/projects/1/internal-access/sandboxes/41",
            Body::empty(),
        ),
        (
            "GET",
            "/sandboxes/sbx_alpha/internal-addresses",
            Body::empty(),
        ),
    ] {
        let response = router(state.clone())
            .layer(Extension(metadata()))
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body)
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn a_reader_cannot_change_sandbox_grants() {
    let (state, audit) = state(untouched(), None);
    let reader = || AuthContext::new_session(user(), Role::Reader);
    assert_eq!(
        create_status(reader(), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(delete_status(reader(), state).await, StatusCode::FORBIDDEN);
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_deployment_token_can_neither_grant_nor_list() {
    let (state, _) = state(untouched(), None);
    assert_eq!(
        create_status(deployment_token(), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete_status(deployment_token(), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    let error = addresses(deployment_token(), state).await.unwrap_err();
    assert_eq!(error.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_user_outside_the_target_project_cannot_grant() {
    let (state, _) = state(untouched(), Some(&[7]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn naming_a_sandbox_you_do_not_own_is_not_found() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![id_row(TARGET)]])
        .append_query_results([Vec::<Row>::new()]);
    let (state, audit) = state(db, Some(&[TARGET]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::NOT_FOUND
    );
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_member_creates_a_sandbox_grant_and_it_is_audited() {
    let (state, audit) = state(create_succeeds(), Some(&[TARGET]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::CREATED
    );
    let operations = audit.operations.lock().unwrap().clone();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].0, SANDBOX_GRANT_CREATED);
    assert!(
        operations[0].1.contains("\"sandbox_id\":\"sbx_alpha\""),
        "{}",
        operations[0].1
    );
}

#[tokio::test]
async fn deleting_a_sandbox_grant_is_audited() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![view_row(41)]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let (state, audit) = state(db, Some(&[TARGET]));
    assert_eq!(
        delete_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(audit.operations.lock().unwrap()[0].0, SANDBOX_GRANT_DELETED);
}

#[tokio::test]
async fn the_sandbox_page_lists_each_address_with_its_curl_command() {
    let address: Row = BTreeMap::from([
        ("grant_id", 41.into()),
        ("project_id", TARGET.into()),
        ("project_name", "Orders API".into()),
        ("project_slug", "orders-api".into()),
        ("environment_id", 3.into()),
        ("environment_name", "Production".into()),
        ("environment_slug", "production".into()),
        ("environment_subdomain", "orders-api-production".into()),
        ("alias_taken", false.into()),
    ]);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![id_row(5)]])
        .append_query_results([vec![count_row(1)]])
        .append_query_results([vec![address]]);
    let (state, _) = state(db, None);
    let response = addresses(AuthContext::new_session(user(), Role::User), state)
        .await
        .expect("listed");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .expect("body");
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("\"hostname\":\"production.orders-api.temps.local\""),
        "{body}"
    );
    assert!(
        body.contains("\"curl_command\":\"curl -sS http://production.orders-api.temps.local/\""),
        "{body}"
    );
    assert!(
        body.contains("\"alias_hostname\":\"orders-api-production.temps.local\""),
        "{body}"
    );
    assert!(
        body.contains(
            "\"alias_curl_command\":\"curl -sS http://orders-api-production.temps.local/\""
        ),
        "{body}"
    );
}

#[tokio::test]
async fn sandbox_errors_are_problem_details_with_the_ids() {
    for (error, status, needle) in [
        (
            InternalAccessError::SandboxNotFound {
                sandbox_id: "sbx_alpha".into(),
            },
            StatusCode::NOT_FOUND,
            "sbx_alpha",
        ),
        (
            InternalAccessError::SandboxGrantNotFound {
                project_id: 1,
                grant_id: 41,
            },
            StatusCode::NOT_FOUND,
            "41",
        ),
        (
            InternalAccessError::DuplicateSandboxGrant {
                target_project_id: 1,
                sandbox_id: "sbx_alpha".into(),
            },
            StatusCode::CONFLICT,
            "sbx_alpha",
        ),
    ] {
        let problem = Problem::from(error);
        assert_eq!(problem.status_code, status);
        let body = serde_json::to_string(&problem.body).expect("serializable");
        assert!(body.contains(needle), "{body}");
    }
}

#[tokio::test]
async fn a_sandbox_agent_cannot_grant_its_own_sandbox_without_the_owner() {
    // The agent inside sbx_alpha acts with its owner's session. Whether the
    // conversation auto-approves, an MCP client confirms, or a page it read
    // told it to, the request carries no `ConfirmedAiWrite` and stops here.
    let (state, audit) = state(untouched(), Some(&[TARGET]));
    let problem = create_as(
        AuthContext::new_session(user(), Role::User),
        state,
        Some(AiToolCall),
        None,
    )
    .await
    .expect_err("an unconfirmed assistant grant must be refused");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    assert_eq!(
        problem.body.get("error_code").and_then(|v| v.as_str()),
        Some("HUMAN_CALLER_REQUIRED")
    );
    assert_eq!(
        problem.body.get("operation").and_then(|v| v.as_str()),
        Some("create_internal_access_sandbox_grant")
    );
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_confirmed_assistant_sandbox_grant_is_audited_as_the_user() {
    let (state, audit) = state(create_succeeds(), Some(&[TARGET]));
    let status = create_as(
        AuthContext::new_session(user(), Role::User),
        state,
        Some(AiToolCall),
        Some(ConfirmedAiWrite),
    )
    .await
    .expect("the owner's confirmation lets the grant through");
    assert_eq!(status, StatusCode::CREATED);
    let operations = audit.operations.lock().unwrap().clone();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].0, SANDBOX_GRANT_CREATED);
    assert!(
        operations[0].1.contains("\"user_id\":9"),
        "{}",
        operations[0].1
    );
}

#[tokio::test]
async fn a_confirmation_cannot_name_someone_elses_sandbox() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![id_row(TARGET)]])
        .append_query_results([Vec::<Row>::new()]);
    let (state, audit) = state(db, Some(&[TARGET]));
    let problem = create_as(
        AuthContext::new_session(user(), Role::User),
        state,
        Some(AiToolCall),
        Some(ConfirmedAiWrite),
    )
    .await
    .expect_err("another user's sandbox stays not found");
    assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_confirmation_does_not_let_a_reader_grant() {
    let (state, audit) = state(untouched(), None);
    let problem = create_as(
        AuthContext::new_session(user(), Role::Reader),
        state,
        Some(AiToolCall),
        Some(ConfirmedAiWrite),
    )
    .await
    .expect_err("a reader cannot grant, confirmed or not");
    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
    assert!(audit.operations.lock().unwrap().is_empty());
}
