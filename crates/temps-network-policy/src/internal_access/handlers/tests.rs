// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who may list, create and delete internal access grants (ADR 100 D3).

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{HeaderMap, Request};
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
use temps_authz::{AuthContext, Permission, Role};
use temps_entities::deployment_tokens::DeploymentTokenPermission;
use temps_entities::users;
use tower::ServiceExt;

use super::*;

const TARGET: i32 = 1;
const CALLER: i32 = 2;

#[derive(Default)]
struct RecordingAudit {
    operations: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AuditLogger for RecordingAudit {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        if let Ok(mut operations) = self.operations.lock() {
            operations.push(operation.operation_type());
        }
        Ok(())
    }
}

/// Team membership: the user may access exactly these projects.
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

/// No query is expected: a guard must answer first.
fn untouched() -> MockDatabase {
    MockDatabase::new(DatabaseBackend::Postgres)
}

fn create_request() -> CreateInternalAccessGrantRequest {
    CreateInternalAccessGrantRequest {
        target_environment_id: None,
        caller_project_id: CALLER,
        caller_environment_id: None,
        description: Some("worker calls the api".to_string()),
    }
}

async fn create_status(auth: AuthContext, state: Arc<InternalAccessAppState>) -> StatusCode {
    match create_internal_access_grant(
        RequireAuth(auth),
        State(state),
        Extension(metadata()),
        Path(TARGET),
        Json(create_request()),
    )
    .await
    {
        Ok(response) => response.into_response().status(),
        Err(problem) => problem.status_code,
    }
}

async fn delete_status(auth: AuthContext, state: Arc<InternalAccessAppState>) -> StatusCode {
    match delete_internal_access_grant(
        RequireAuth(auth),
        State(state),
        Extension(metadata()),
        Path((TARGET, 33)),
    )
    .await
    {
        Ok(response) => response.into_response().status(),
        Err(problem) => problem.status_code,
    }
}

async fn list_status(
    auth: AuthContext,
    state: Arc<InternalAccessAppState>,
    project_id: i32,
) -> StatusCode {
    match list_internal_access_grants(
        RequireAuth(auth),
        State(state),
        Path(project_id),
        Query(ListInternalAccessQuery {
            environment_id: None,
            page: None,
            page_size: None,
        }),
    )
    .await
    {
        Ok(response) => response.into_response().status(),
        Err(problem) => problem.status_code,
    }
}

fn view_row(id: i32) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([
        ("id", id.into()),
        ("target_project_id", TARGET.into()),
        ("target_project_name", "Orders API".into()),
        ("target_project_slug", "orders-api".into()),
        ("target_environment_id", Option::<i32>::None.into()),
        ("target_environment_name", Option::<String>::None.into()),
        ("caller_project_id", CALLER.into()),
        ("caller_project_name", "Checkout Worker".into()),
        ("caller_project_slug", "checkout-worker".into()),
        ("caller_environment_id", Option::<i32>::None.into()),
        ("caller_environment_name", Option::<String>::None.into()),
        (
            "description",
            Some("worker calls the api".to_string()).into(),
        ),
        ("created_by", Some(9).into()),
        ("created_at", chrono::Utc::now().into()),
    ])
}

fn id_row(id: i32) -> BTreeMap<&'static str, sea_orm::Value> {
    BTreeMap::from([("id", id.into())])
}

/// The queries a successful create runs, in order.
fn create_succeeds() -> MockDatabase {
    let inserted = BTreeMap::from([
        ("id", 33.into()),
        ("target_project_id", TARGET.into()),
        ("target_environment_id", Option::<i32>::None.into()),
        ("caller_project_id", CALLER.into()),
        ("caller_environment_id", Option::<i32>::None.into()),
        (
            "description",
            Some("worker calls the api".to_string()).into(),
        ),
        ("created_by", Some(9).into()),
        ("created_at", chrono::Utc::now().into()),
    ]);
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![id_row(TARGET)]])
        .append_query_results([vec![id_row(CALLER)]])
        .append_query_results([vec![BTreeMap::from([("n", 0i64.into())])]])
        .append_query_results([vec![inserted]])
        .append_query_results([vec![view_row(33)]])
}

#[tokio::test]
async fn every_endpoint_requires_authentication() {
    let (state, _) = state(untouched(), None);
    for (method, uri, body) in [
        ("GET", "/projects/1/internal-access", Body::empty()),
        (
            "POST",
            "/projects/1/internal-access",
            Body::from(r#"{"caller_project_id":2}"#),
        ),
        ("DELETE", "/projects/1/internal-access/33", Body::empty()),
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
async fn a_reader_can_list_but_not_change_grants() {
    let (state_for_list, _) = state(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![id_row(TARGET)]])
            .append_query_results([vec![BTreeMap::from([("n", 0i64.into())])]])
            .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()]),
        None,
    );
    assert_eq!(
        list_status(
            AuthContext::new_session(user(), Role::Reader),
            state_for_list,
            TARGET
        )
        .await,
        StatusCode::OK
    );

    let (state, audit) = state(untouched(), None);
    assert_eq!(
        create_status(
            AuthContext::new_session(user(), Role::Reader),
            state.clone()
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete_status(AuthContext::new_session(user(), Role::Reader), state).await,
        StatusCode::FORBIDDEN
    );
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_read_only_api_key_cannot_create() {
    let key = AuthContext::new_api_key(
        user(),
        None,
        Some(vec![Permission::ProjectsRead]),
        "read-only".to_string(),
        1,
    );
    let (state, _) = state(untouched(), None);
    assert_eq!(create_status(key, state).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_deployment_token_can_neither_change_nor_read_grants() {
    let token = || {
        AuthContext::new_deployment_token(
            TARGET,
            None,
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        )
    };
    let (state, _) = state(untouched(), None);
    assert_eq!(
        create_status(token(), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete_status(token(), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        list_status(token(), state.clone(), TARGET).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        list_status(token(), state, 5).await,
        StatusCode::FORBIDDEN,
        "nor another project's"
    );
}

#[tokio::test]
async fn a_member_of_another_team_cannot_change_the_targets_grants() {
    // Member of the caller project only.
    let (state, _) = state(untouched(), Some(&[CALLER]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn naming_a_project_you_cannot_see_is_refused() {
    // Member of the target project only.
    let (state, audit) = state(untouched(), Some(&[TARGET]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::FORBIDDEN
    );
    assert!(audit.operations.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_member_of_both_projects_creates_a_grant_and_it_is_audited() {
    let (state, audit) = state(create_succeeds(), Some(&[TARGET, CALLER]));
    assert_eq!(
        create_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::CREATED
    );
    assert_eq!(
        *audit.operations.lock().unwrap(),
        vec![GRANT_CREATED.to_string()]
    );
}

#[tokio::test]
async fn deleting_a_grant_is_audited() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![view_row(33)]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    let (state, audit) = state(db, Some(&[TARGET]));
    assert_eq!(
        delete_status(AuthContext::new_session(user(), Role::User), state).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        *audit.operations.lock().unwrap(),
        vec![GRANT_DELETED.to_string()]
    );
}

#[tokio::test]
async fn errors_are_problem_details_with_the_ids() {
    let problem = Problem::from(InternalAccessError::GrantNotFound {
        project_id: 1,
        grant_id: 33,
    });
    assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    let body = serde_json::to_string(&problem.body).expect("serializable");
    assert!(body.contains("33") && body.contains("project 1"), "{body}");

    for (error, status) in [
        (
            InternalAccessError::SelfGrant { project_id: 1 },
            StatusCode::BAD_REQUEST,
        ),
        (
            InternalAccessError::DuplicateGrant {
                target_project_id: 1,
                caller_project_id: 2,
            },
            StatusCode::CONFLICT,
        ),
        (
            InternalAccessError::TooManyGrants {
                project_id: 1,
                limit: 100,
            },
            StatusCode::CONFLICT,
        ),
        (
            InternalAccessError::EnvironmentNotFound {
                project_id: 1,
                environment_id: 4,
            },
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(Problem::from(error).status_code, status);
    }
}
