// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::{broker::UnavailableTokenlessBroker, service::ServiceAccessService};
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Request},
};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Mutex;
use temps_authz::{
    permissions::{Permission, Role},
    AuthContext,
};
use temps_entities::users;
use tower::ServiceExt;
#[derive(Default)]
struct Audit(Mutex<Vec<String>>);
#[async_trait::async_trait]
impl temps_core::AuditLogger for Audit {
    async fn create_audit_log(
        &self,
        op: &dyn temps_core::AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.0.lock().unwrap().push(op.serialize()?);
        Ok(())
    }
}
fn test_user() -> users::Model {
    let now = chrono::Utc::now();
    users::Model {
        id: 1,
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

fn test_metadata() -> RequestMetadata {
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

fn state(db: MockDatabase, audit: Arc<Audit>) -> Arc<AccessAppState> {
    Arc::new(AccessAppState {
        service: Arc::new(ServiceAccessService::new(
            Arc::new(db.into_connection()),
            Arc::new(temps_core::EncryptionService::new(&"ab".repeat(32)).unwrap()),
            Arc::new(UnavailableTokenlessBroker),
        )),
        audit_service: audit,
        project_access_checker: None,
    })
}
fn routes() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("GET", "/projects/7/service-access/capability", ""),
        ("GET", "/projects/7/access-services", ""),
        (
            "POST",
            "/projects/7/access-services",
            r#"{"name":"API","policy":{"origin":"https://api.example.com","path_prefixes":["/"],"methods":["GET"]},"max_ttl_seconds":900,"enabled":true}"#,
        ),
        (
            "PUT",
            "/projects/7/access-services/svc_test",
            r#"{"name":"API","policy":{"origin":"https://api.example.com","path_prefixes":["/"],"methods":["GET"]},"max_ttl_seconds":900,"enabled":true,"expected_version":1}"#,
        ),
        ("GET", "/projects/7/sandboxes/sbx_test/service-grants", ""),
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/service-grants",
            r#"{"service_id":"svc_test","service_version":1,"ttl_seconds":900}"#,
        ),
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/service-grants/grant_test/revoke",
            r#"{"expected_version":1}"#,
        ),
        ("GET", "/projects/7/access-services/svc_test", ""),
    ]
}
async fn send(
    method: &str,
    path: &str,
    body: &str,
    auth: Option<AuthContext>,
    state: Arc<AccessAppState>,
) -> axum::response::Response {
    let mut router = configure_routes()
        .with_state(state)
        .layer(Extension(test_metadata()));
    if let Some(auth) = auth {
        router = router.layer(Extension(auth));
    }
    router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn every_route_requires_authentication_and_returns_problem_details() {
    for (method, path, body) in routes() {
        let response = send(
            method,
            path,
            body,
            None,
            state(
                MockDatabase::new(DatabaseBackend::Postgres),
                Arc::new(Audit::default()),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("application/problem+json"));
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["title"], "Authentication Required");
        assert!(problem["detail"].is_string());
    }
}
#[tokio::test]
async fn every_route_enforces_permissions_and_rejects_deployment_tokens() {
    for deployment in [false, true] {
        for (method, path, body) in routes() {
            let auth = if deployment {
                AuthContext::new_deployment_token(8, None, None, 1, "app".into(), vec![])
            } else {
                AuthContext::new_api_key(test_user(), None, Some(vec![]), "key".into(), 1)
            };
            let response = send(
                method,
                path,
                body,
                Some(auth),
                state(
                    MockDatabase::new(DatabaseBackend::Postgres),
                    Arc::new(Audit::default()),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
}
#[tokio::test]
async fn read_only_key_cannot_save_or_revoke() {
    for (method, path, body) in routes()
        .into_iter()
        .filter(|(method, _, _)| *method != "GET")
    {
        let auth = AuthContext::new_api_key(
            test_user(),
            None,
            Some(vec![Permission::ProjectsRead, Permission::SandboxesRead]),
            "read-key".into(),
            1,
        );
        assert_eq!(
            send(
                method,
                path,
                body,
                Some(auth),
                state(
                    MockDatabase::new(DatabaseBackend::Postgres),
                    Arc::new(Audit::default())
                )
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
}
#[tokio::test]
async fn capability_explains_missing_integration_without_claiming_connection() {
    let response = send(
        "GET",
        "/projects/7/service-access/capability",
        "",
        Some(AuthContext::new_session(test_user(), Role::Admin)),
        state(
            MockDatabase::new(DatabaseBackend::Postgres),
            Arc::new(Audit::default()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cap: AccessCapability =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert!(!cap.configured);
    assert!(cap.can_save_requests);
    assert_eq!(cap.state, AccessCapabilityState::IntegrationUnavailable);
}
#[tokio::test]
async fn problem_mapping_never_leaks_database_sources() {
    let problem: Problem = AccessError::Database {
        operation: "save service",
        project_id: 7,
        source: sea_orm::DbErr::Custom("secret-driver-payload".into()),
    }
    .into();
    assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    let encoded = String::from_utf8(
        to_bytes(problem.into_response().into_body(), 8192)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!encoded.contains("secret-driver-payload"));
}
#[tokio::test]
async fn create_returns_masked_service_and_audits_only_metadata() {
    use std::collections::BTreeMap;
    let now = chrono::Utc::now();
    let model = temps_entities::sandbox_access_services::Model {
        id: "svc_test".into(),
        project_id: 7,
        name: "API".into(),
        policy: serde_json::json!({"origin":"https://api.example.com","path_prefixes":["/"],"methods":["GET"]}),
        max_ttl_seconds: 900,
        enabled: true,
        credential_encrypted: Some("ciphertext".into()),
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![BTreeMap::from([(
            "id".to_string(),
            sea_orm::Value::Int(Some(7)),
        )])]])
        .append_query_results([vec![model]]);
    let audit = Arc::new(Audit::default());
    let body = r#"{"name":"API","policy":{"origin":"https://api.example.com","path_prefixes":["/"],"methods":["GET"]},"max_ttl_seconds":900,"enabled":true,"credential":"test-bearer-value"}"#;
    let response = send(
        "POST",
        "/projects/7/access-services",
        body,
        Some(AuthContext::new_session(test_user(), Role::Admin)),
        state(db, audit.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body =
        String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
    assert!(body.contains("has_credential"));
    assert!(!body.contains("ciphertext"));
    assert!(!body.contains("test-bearer-value"));
    let events = audit.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("SANDBOX_ACCESS_SERVICE_CREATED"));
    assert!(!events[0].contains("test-bearer-value"));
}

use chrono::{Duration, Utc};
use temps_entities::{
    sandbox_access_services as services, sandbox_service_grants as grants, sandboxes,
};

fn policy() -> SandboxAccessPolicy {
    SandboxAccessPolicy {
        origin: "https://api.example.com".into(),
        path_prefixes: vec!["/v1".into()],
        methods: vec![AccessMethod::Get],
    }
}
fn row() -> services::Model {
    services::Model {
        id: "svc_test".into(),
        project_id: 7,
        name: "API".into(),
        policy: serde_json::to_value(policy()).unwrap(),
        max_ttl_seconds: 900,
        enabled: true,
        credential_encrypted: None,
        version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}
fn grant() -> grants::Model {
    let now = Utc::now();
    grants::Model {
        id: "grant_test".into(),
        project_id: 7,
        service_id: "svc_test".into(),
        sandbox_id: 42,
        service_version: 1,
        policy_snapshot: serde_json::to_value(policy()).unwrap(),
        created_by: Some(1),
        version: 1,
        created_at: now,
        expires_at: now + Duration::seconds(60),
        revoked_at: None,
    }
}
fn sandbox() -> sandboxes::Model {
    sandboxes::Model {
        id: 42,
        public_id: "sbx_test".into(),
        user_id: Some(1),
        agent_run_id: None,
        name: "test".into(),
        status: "running".into(),
        lifecycle: "workspace".into(),
        project_id: Some(7),
        source_repo_url: None,
        image: None,
        work_dir: "/workspace".into(),
        timeout_secs: 900,
        metadata: None,
        backend: Some("docker".into()),
        node_id: None,
        egress_policy: Default::default(),
        created_at: Utc::now(),
        last_activity_at: Utc::now(),
        expires_at: Utc::now() + Duration::seconds(900),
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: vec![],
    }
}
#[tokio::test]
async fn management_routes_return_typed_success_and_audit_each_write() {
    use std::collections::BTreeMap;
    let project = || BTreeMap::from([("id".to_string(), sea_orm::Value::Int(Some(7)))]);
    let count = || BTreeMap::from([("num_items".to_string(), sea_orm::Value::BigInt(Some(1)))]);
    let mut updated = row();
    updated.version = 2;
    let mut revoked = grant();
    revoked.version = 2;
    revoked.revoked_at = Some(Utc::now());
    let cases = vec![
        (
            1,
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project()]])
                .append_query_results([vec![count()]])
                .append_query_results([vec![row()]]),
            200,
            false,
        ),
        (
            3,
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project()]])
                .append_query_results([vec![row()], vec![updated]]),
            200,
            true,
        ),
        (
            4,
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![sandbox()]])
                .append_query_results([vec![count()]])
                .append_query_results([vec![(grant(), row())]]),
            200,
            false,
        ),
        (
            5,
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![row()]])
                .append_query_results([vec![sandbox()]])
                .append_query_results([vec![grant()]]),
            201,
            true,
        ),
        (
            6,
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![sandbox()]])
                .append_query_results([vec![grant()]])
                .append_query_results([vec![row()]])
                .append_query_results([vec![revoked]]),
            200,
            true,
        ),
    ];
    for (index, db, status, writes) in cases {
        let (method, path, body) = routes()[index];
        let audit = Arc::new(Audit::default());
        let response = send(
            method,
            path,
            body,
            Some(AuthContext::new_session(test_user(), Role::Admin)),
            state(db, audit.clone()),
        )
        .await;
        assert_eq!(response.status().as_u16(), status, "{method} {path}");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 32768).await.unwrap()).unwrap();
        if path.contains("service-grants") {
            let grant = if method == "GET" {
                &body["items"][0]
            } else {
                &body
            };
            assert_eq!(grant["connected"], false);
            assert_eq!(grant["sandbox_id"], "sbx_test");
        }
        assert_eq!(audit.0.lock().unwrap().len(), usize::from(writes));
    }
}

struct ProjectPermissions {
    sandbox_access: bool,
    fail: bool,
}
#[async_trait::async_trait]
impl temps_core::ProjectAccessChecker for ProjectPermissions {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(true)
    }
    async fn effective_project_permissions(
        &self,
        user_id: i32,
        project_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
        assert_eq!((user_id, project_id), (1, 7));
        if self.fail {
            return Err(Box::new(std::io::Error::other("permission lookup failed")));
        }
        let mut permissions = vec!["projects:read".into(), "projects:write".into()];
        if self.sandbox_access {
            permissions.extend(["sandboxes:read".into(), "sandboxes:write".into()]);
        }
        Ok(Some(permissions))
    }
}

#[tokio::test]
async fn grant_routes_honor_project_permission_narrowing_and_checker_failures() {
    for (sandbox_access, fail, expected) in [
        (false, false, StatusCode::FORBIDDEN),
        (true, false, StatusCode::NOT_FOUND),
        (true, true, StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        for (method, path, body) in routes()
            .into_iter()
            .filter(|(_, path, _)| path.contains("/sandboxes/"))
        {
            let audit = Arc::new(Audit::default());
            let mut state = state(
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results([Vec::<temps_entities::sandboxes::Model>::new()]),
                audit.clone(),
            );
            Arc::get_mut(&mut state).unwrap().project_access_checker =
                Some(Arc::new(ProjectPermissions {
                    sandbox_access,
                    fail,
                }));
            let auth = AuthContext::new_api_key(
                test_user(),
                Some(Role::User),
                Some(vec![
                    Permission::ProjectsRead,
                    Permission::ProjectsWrite,
                    Permission::SandboxesRead,
                    Permission::SandboxesWrite,
                ]),
                "scoped-key".into(),
                1,
            );
            let response = send(method, path, body, Some(auth), state).await;
            assert_eq!(response.status(), expected, "{method} {path}");
            if expected == StatusCode::FORBIDDEN {
                let body = to_bytes(response.into_body(), 8192).await.unwrap();
                let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(problem["title"], "Project Permission Denied");
                assert!(problem["detail"].as_str().unwrap().contains("sandboxes:"));
            }
            assert!(audit.0.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn single_service_read_is_masked_and_missing_is_a_problem() {
    for found in [true, false] {
        let mut model = row();
        model.credential_encrypted = Some("ciphertext-must-not-escape".into());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "id".to_string(),
                sea_orm::Value::Int(Some(7)),
            )])]])
            .append_query_results([if found { vec![model] } else { vec![] }]);
        let response = send(
            "GET",
            "/projects/7/access-services/svc_test",
            "",
            Some(AuthContext::new_session(test_user(), Role::Admin)),
            state(db, Arc::new(Audit::default())),
        )
        .await;
        assert_eq!(
            response.status(),
            if found {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
        let body = String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec())
            .unwrap();
        assert!(!body.contains("ciphertext-must-not-escape"));
        if found {
            assert!(body.contains("has_credential"));
        } else {
            assert!(body.contains("svc_test"));
            assert!(body.contains("detail"));
        }
    }
}
