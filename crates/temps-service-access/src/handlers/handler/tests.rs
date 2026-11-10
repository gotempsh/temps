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
mod session_success;
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
    state_with_db(Arc::new(db.into_connection()), audit)
}
fn state_with_db(db: Arc<sea_orm::DatabaseConnection>, audit: Arc<Audit>) -> Arc<AccessAppState> {
    let encryption = Arc::new(temps_core::EncryptionService::new(&"ab".repeat(32)).unwrap());
    let service = Arc::new(ServiceAccessService::new(
        db.clone(),
        encryption.clone(),
        Arc::new(UnavailableTokenlessBroker),
    ));
    let coordinator = Arc::new(crate::coordinator::ServiceAccessCoordinator::new(
        db,
        encryption,
        audit.clone(),
        service.clone(),
        Arc::new(crate::transport::WorkerTransport::new(
            crate::coordinator::tests::provider::Provider::new(),
        )),
        None,
        Arc::new(crate::github::NoGithub),
    ));
    Arc::new(AccessAppState {
        coordinator,
        service,
        audit_service: audit,
        project_access_checker: None,
    })
}
fn routes() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut routes = vec![
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
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/service-grants/approve",
            r#"{"request_id":"00000000-0000-4000-8000-000000000001","service_id":"svc_test","service_version":1,"ttl_seconds":900}"#,
        ),
        ("GET", "/projects/7/access-vaults", ""),
        ("GET", "/projects/7/access-vaults/vault_test", ""),
        (
            "POST",
            "/projects/7/access-vaults",
            r#"{"name":"Vault","service_ids":[]}"#,
        ),
        (
            "PUT",
            "/projects/7/access-vaults/vault_test",
            r#"{"name":"Vault","service_ids":[],"expected_version":1}"#,
        ),
        ("GET", "/projects/7/access-vaults/vault_test/bundles", ""),
        (
            "POST",
            "/projects/7/access-vaults/vault_test/bundles",
            r#"{"name":"Bundle","service_ids":["svc_test"],"ttl_seconds":900}"#,
        ),
        (
            "PUT",
            "/projects/7/access-vaults/vault_test/bundles/bundle_test",
            r#"{"name":"Bundle","service_ids":["svc_test"],"ttl_seconds":900,"expected_version":1}"#,
        ),
        ("GET", "/projects/7/access-sessions", ""),
        ("GET", "/projects/7/access-sessions/session_test", ""),
        (
            "POST",
            "/projects/7/access-sessions/session_test/revoke",
            "",
        ),
        (
            "POST",
            "/projects/7/access-vaults/vault_test/sessions",
            r#"{"request_id":"00000000-0000-4000-8000-000000000001","bundle_id":"bundle_test","bundle_version":1,"consumer":{"kind":"sandbox","sandbox_id":"sbx_test"},"ttl_seconds":60,"services":[{"service_id":"svc_test","version":1}]}"#,
        ),
        ("GET", "/projects/7/sandboxes/sbx_test/services", ""),
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/services",
            r#"{"service_id":"svc_test","service_version":1}"#,
        ),
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/services/access_test/approve",
            r#"{"expected_version":1,"service_version":1}"#,
        ),
        (
            "POST",
            "/projects/7/sandboxes/sbx_test/services/access_test/revoke",
            r#"{"expected_version":1}"#,
        ),
    ];
    let global: Vec<_> = routes
        .iter()
        .filter(|(_, path, _)| path.starts_with("/projects/7/access-"))
        .map(|(method, path, body)| (*method, &path[11..], *body))
        .collect();
    routes.extend(global);
    routes.extend([
        ("GET", "/service-access/capability", ""),
        ("GET", "/access-vaults/vault_test/consumers/sbx_test", ""),
        ("DELETE", "/access-services/svc_test", ""),
        ("GET", "/sandboxes/sbx_test/access-vaults", ""),
        ("GET", "/projects/7/linked-vaults", ""),
        ("GET", "/access-vaults/vault_test/links", ""),
        (
            "POST",
            "/access-vaults/vault_test/links",
            r#"{"sandbox_id":"sbx_test"}"#,
        ),
        ("DELETE", "/access-vaults/vault_test/links/link_test", ""),
        ("DELETE", "/access-vaults/vault_test", ""),
        (
            "DELETE",
            "/access-vaults/vault_test/bundles/bundle_test",
            "",
        ),
    ]);
    routes
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
    assert!(cap.can_manage_services);
    assert!(cap.can_manage_requests);
    assert_eq!(cap.state, AccessCapabilityState::IntegrationUnavailable);
}
#[tokio::test]
async fn problem_mapping_never_leaks_database_sources() {
    for error in [
        AccessError::Database {
            operation: "save service",
            project_id: Some(7),
            source: sea_orm::DbErr::Custom("secret-driver-payload".into()),
        },
        AccessError::Delivery {
            project_id: Some(7),
            sandbox_id: "sbx_test".into(),
            source: Box::new(crate::delivery::DeliveryError::Database {
                operation: "enqueue policy",
                subject: "test runtime".into(),
                source: sea_orm::DbErr::Custom("secret-driver-payload".into()),
            }),
        },
    ] {
        let problem: Problem = error.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
        let encoded = String::from_utf8(
            to_bytes(problem.into_response().into_body(), 8192)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(encoded.contains("project 7"));
        assert!(!encoded.contains("secret-driver-payload"));
    }
}
#[tokio::test]
async fn create_returns_masked_service_and_audits_only_metadata() {
    use std::collections::BTreeMap;
    let now = chrono::Utc::now();
    let model = temps_entities::sandbox_access_services::Model {
        retired_at: None,
        id: "svc_test".into(),
        project_id: Some(7),
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
        provider: None,
        github_repository: None,
        origin: "https://api.example.com".into(),
        path_prefixes: vec!["/v1".into()],
        methods: vec![AccessMethod::Get],
    }
}
fn row() -> services::Model {
    services::Model {
        retired_at: None,
        id: "svc_test".into(),
        project_id: Some(7),
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
        assignment_id: None,
        id: "grant_test".into(),
        project_id: Some(7),
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
                .append_query_results([vec![(grant(), row())]])
                .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]),
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

#[tokio::test]
async fn capability_reports_effective_write_permissions() {
    for (role, manage_services, manage_requests) in [
        (Role::Reader, false, false),
        (Role::User, false, true),
        (Role::PlatformAdmin, false, false),
        (Role::Admin, true, true),
    ] {
        let response = send(
            "GET",
            "/projects/7/service-access/capability",
            "",
            Some(AuthContext::new_session(test_user(), role.clone())),
            state(
                MockDatabase::new(DatabaseBackend::Postgres),
                Arc::new(Audit::default()),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cap: AccessCapability =
            serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(cap.can_manage_services, manage_services, "{role:?}");
        assert_eq!(cap.can_manage_requests, manage_requests, "{role:?}");
    }
    for (sandbox_access, fail) in [(false, false), (true, false), (true, true)] {
        let mut state = state(
            MockDatabase::new(DatabaseBackend::Postgres),
            Arc::new(Audit::default()),
        );
        Arc::get_mut(&mut state).unwrap().project_access_checker =
            Some(Arc::new(ProjectPermissions {
                sandbox_access,
                fail,
            }));
        let response = send(
            "GET",
            "/projects/7/service-access/capability",
            "",
            Some(AuthContext::new_session(test_user(), Role::User)),
            state,
        )
        .await;
        if fail {
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            let cap: AccessCapability =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert!(!cap.can_manage_services);
            assert_eq!(cap.can_manage_requests, sandbox_access);
        }
    }
    let problem = permission_available(Err(temps_core::problemdetails::new(
        StatusCode::INTERNAL_SERVER_ERROR,
    )))
    .unwrap_err();
    assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn assistant_cannot_approve_access_even_with_admin_permissions() {
    let audit = Arc::new(Audit::default());
    let response = configure_routes()
        .with_state(state(
            MockDatabase::new(DatabaseBackend::Postgres),
            audit.clone(),
        ))
        .layer(Extension(test_metadata()))
        .layer(Extension(AuthContext::new_api_key(
            test_user(),
            Some(Role::Admin),
            None,
            "key".into(),
            1,
        )))
        .layer(Extension(temps_core::ai_tool_call::AiToolCall))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects/7/sandboxes/sbx_test/service-grants/approve")
                .header("content-type", "application/json")
                .body(Body::from(routes()[8].2))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["error_code"], "HUMAN_CALLER_REQUIRED");
    assert!(audit.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn approval_rejects_invalid_consent_before_contacting_a_worker() {
    let response = send(
        "POST", "/projects/7/sandboxes/sbx_test/service-grants/approve",
        r#"{"request_id":"00000000-0000-0000-0000-000000000000","service_id":"svc_test","service_version":1,"ttl_seconds":900}"#,
        Some(AuthContext::new_api_key(test_user(), Some(Role::Admin), None, "key".into(), 1)),
        state(MockDatabase::new(DatabaseBackend::Postgres), Arc::new(Audit::default())),
    ).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(problem["detail"]
        .as_str()
        .unwrap()
        .contains("non-nil request ID"));
}

#[tokio::test]
async fn coordinator_problem_mapping_preserves_context_and_redacts_driver_sources() {
    use crate::{
        coordinator::CoordinatorError, delivery::DeliveryError, transport::TransportError,
    };
    for (error, expected) in [
        (
            CoordinatorError::Database {
                sandbox_id: "sbx_test".into(),
                operation: "approve",
                source: sea_orm::DbErr::Custom("secret-driver-payload".into()),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            CoordinatorError::Delivery(DeliveryError::Database {
                subject: "sbx_test".into(),
                operation: "queue approval",
                source: sea_orm::DbErr::Custom("secret-driver-payload".into()),
            }),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            CoordinatorError::Invalid {
                sandbox_id: "sbx_test".into(),
                operation: "approve",
                reason: "sandbox changed",
            },
            StatusCode::CONFLICT,
        ),
        (
            CoordinatorError::Transport(TransportError::Timeout {
                sandbox_id: "sbx_test".into(),
                operation: "inspect",
                seconds: 10,
            }),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            CoordinatorError::Access(AccessError::NotFound {
                kind: "sandbox",
                id: "sbx_test".into(),
                project_id: Some(7),
            }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let problem: Problem = error.into();
        assert_eq!(problem.status_code, expected);
        let body = String::from_utf8(
            to_bytes(problem.into_response().into_body(), 8192)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains("sbx_test"));
        assert!(!body.contains("secret-driver-payload"));
    }
}

#[tokio::test]
async fn postgres_approval_route_is_fresh_scoped_idempotent_and_audited() {
    use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};
    if !temps_test_utils::external_database_configured()
        && !temps_test_utils::docker_available().await
    {
        eprintln!("Skipping sandbox approval API: Docker unavailable");
        return;
    }
    let mut test = temps_test_utils::TestDatabase::with_migrations()
        .await
        .unwrap();
    let db = test.connection_arc();
    db.execute_unprepared("INSERT INTO users (id,name,email) VALUES (9801,'Test user','user@example.com'); INSERT INTO projects (id,name,repo_name,repo_owner,directory,main_branch,preset,slug,created_at,updated_at) VALUES (9801,'Approval','repo','owner','/','main','nodejs','approval',now(),now()); INSERT INTO sandboxes (id,public_id,name,user_id,project_id,status,backend) VALUES (9801,'sbx_dispatch','approval',9801,9801,'running','docker')").await.unwrap();
    let audit = Arc::new(Audit::default());
    let state = state_with_db(db.clone(), audit.clone());
    let service = state
        .service
        .save_service(
            9801,
            None,
            SaveAccessService {
                vault_id: None,
                name: "API".into(),
                policy: SandboxAccessPolicy {
                    provider: None,
                    github_repository: None,
                    origin: "https://api.example.com".into(),
                    path_prefixes: vec!["/user".into()],
                    methods: vec![AccessMethod::Get],
                },
                max_ttl_seconds: 900,
                enabled: true,
                credential: Some(temps_core::secret_string::SecretString::new(
                    "fixture-credential",
                )),
                clear_credential: false,
                expected_version: None,
            },
        )
        .await
        .unwrap();
    let body = serde_json::json!({"request_id": uuid::Uuid::new_v4(), "service_id": service.id, "service_version": 1, "ttl_seconds": 900}).to_string();
    let path = "/projects/9801/sandboxes/sbx_dispatch/service-grants/approve";
    let actor = || {
        let mut user = test_user();
        user.id = 9801;
        AuthContext::new_api_key(
            user,
            None,
            Some(vec![Permission::ProjectsWrite, Permission::SandboxesWrite]),
            "key".into(),
            1,
        )
    };
    let mut receipt = None;
    for _ in 0..2 {
        let response = send("POST", path, &body, Some(actor()), state.clone()).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let raw = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("fixture-credential"));
        let received: ServiceGrantApproval = serde_json::from_slice(&raw).unwrap();
        if let Some((id, expiry)) = &receipt {
            assert_eq!(&received.grant_id, id);
            assert_eq!(&received.expires_at, expiry);
        } else {
            receipt = Some((received.grant_id, received.expires_at));
        }
    }
    assert_eq!(
        temps_entities::sandbox_service_grants::Entity::find()
            .count(db.as_ref())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        temps_entities::sandbox_access_authorizations::Entity::find()
            .count(db.as_ref())
            .await
            .unwrap(),
        1
    );
    let runtime = temps_entities::sandbox_access_runtimes::Entity::find()
        .one(db.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.applied_version, 0,
        "an accepted approval is not an enforcement receipt"
    );
    assert!(runtime.lease_expires_at.is_none());
    assert_eq!(
        state.coordinator.dispatch_one().await.unwrap(),
        Some(crate::coordinator::DispatchOutcome::Applied)
    );
    // Neither another project nor another owner can use this sandbox.
    for (path, auth) in [
        (
            "/projects/9802/sandboxes/sbx_dispatch/service-grants/approve",
            actor(),
        ),
        (
            path,
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::ProjectsWrite, Permission::SandboxesWrite]),
                "other-key".into(),
                2,
            ),
        ),
    ] {
        assert_eq!(
            send("POST", path, &body, Some(auth), state.clone())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    {
        let events = audit.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.contains("SANDBOX_SERVICE_GRANT_APPROVED"))
                .count(),
            2
        );
        assert!(events.iter().all(|e| !e.contains("fixture-credential")));
    }
    // The chat marker uses the same user-scoped write path as the console.
    // Declaration alone is inert; explicit approval and revocation are audited.
    let chat = configure_routes()
        .with_state(state.clone())
        .layer(Extension(test_metadata()))
        .layer(Extension(actor()))
        .layer(Extension(temps_core::ai_tool_call::AiToolCall));
    let mut access_id = String::new();
    for (operation, expected_version, expected_status) in [
        (
            "",
            0,
            crate::assignment_types::SandboxServiceStatus::PendingApproval,
        ),
        (
            "approve",
            1,
            crate::assignment_types::SandboxServiceStatus::Approved,
        ),
        (
            "revoke",
            2,
            crate::assignment_types::SandboxServiceStatus::Revoked,
        ),
    ] {
        let base = "/projects/9801/sandboxes/sbx_dispatch/services";
        let (uri, body) = if operation.is_empty() {
            (
                base.to_string(),
                serde_json::json!({"service_id":service.id,"service_version":1}),
            )
        } else if operation == "approve" {
            (
                format!("{base}/{access_id}/{operation}"),
                serde_json::json!({"expected_version":expected_version,"service_version":1}),
            )
        } else {
            (
                format!("{base}/{access_id}/{operation}"),
                serde_json::json!({"expected_version":expected_version}),
            )
        };
        let response = chat
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let raw = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("fixture-credential"));
        let result: crate::assignment_types::SandboxServiceResponse =
            serde_json::from_slice(&raw).unwrap();
        assert_eq!(result.status, expected_status);
        assert!(!result.connected);
        assert!(result.expires_at.is_none());
        access_id = result.id;
    }
    {
        let events = audit.0.lock().unwrap();
        for op in [
            "SANDBOX_SERVICE_ACCESS_REQUESTED",
            "SANDBOX_SERVICE_ACCESS_APPROVED",
            "SANDBOX_SERVICE_ACCESS_REVOKED",
        ] {
            assert_eq!(events.iter().filter(|e| e.contains(op)).count(), 1);
        }
        assert!(events.iter().all(|e| !e.contains("fixture-credential")));
    }
    test.cleanup().await;
}

#[tokio::test]
async fn github_policy_writes_require_git_read_permission_before_database_access() {
    let body = serde_json::json!({"name":"GitHub", "policy":crate::github::tests::policy(GithubAccessProtocol::Git), "max_ttl_seconds":900,"enabled":true});
    for git_permissions in [
        vec![],
        vec![Permission::GitRepositoriesRead],
        vec![Permission::GitConnectionsRead],
    ] {
        for (method, path) in [
            ("POST", "/projects/7/access-services"),
            ("PUT", "/projects/7/access-services/svc_test"),
        ] {
            let response = send(
                method,
                path,
                &body.to_string(),
                Some(AuthContext::new_api_key(
                    test_user(),
                    None,
                    Some(
                        [
                            vec![Permission::ProjectsWrite, Permission::SandboxesWrite],
                            git_permissions.clone(),
                        ]
                        .concat(),
                    ),
                    "key".into(),
                    1,
                )),
                state(
                    MockDatabase::new(DatabaseBackend::Postgres),
                    Arc::new(Audit::default()),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
}

#[tokio::test]
async fn chat_service_approval_uses_user_authorization_instead_of_a_screen_gate() {
    let response = configure_routes()
        .with_state(state(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom("test storage unavailable".into())]),
            Arc::new(Audit::default()),
        ))
        .layer(Extension(test_metadata()))
        .layer(Extension(AuthContext::new_api_key(
            test_user(),
            Some(Role::Admin),
            None,
            "key".into(),
            1,
        )))
        .layer(Extension(temps_core::ai_tool_call::AiToolCall))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects/7/sandboxes/sbx_test/services/access_test/approve")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"expected_version":1,"service_version":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // It reaches scoped storage, rather than rejecting the trusted assistant
    // marker. Unauthorized users are exercised by the shared route guard tests.
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("HUMAN_CALLER_REQUIRED"));
}

#[tokio::test]
async fn approval_permissions_are_fenced_to_the_exact_reviewed_policy() {
    use std::collections::BTreeMap;
    use temps_entities::{access_vaults, sandbox_service_assignments};
    // The key belongs to an admin, but its own grant excludes Git permissions.
    // A future requested version must not reach a second read that could see Git.
    for route in ["assignment", "grant", "session"] {
        for (git_policy, requested_version, git_permissions, expected) in [
            (false, 2, vec![], StatusCode::CONFLICT),
            (true, 1, vec![], StatusCode::FORBIDDEN),
            (
                true,
                1,
                vec![Permission::GitRepositoriesRead],
                StatusCode::FORBIDDEN,
            ),
            (
                true,
                1,
                vec![Permission::GitConnectionsRead],
                StatusCode::FORBIDDEN,
            ),
        ] {
            let now = Utc::now();
            let mut service = row();
            if git_policy {
                service.policy =
                    serde_json::to_value(crate::github::tests::policy(GithubAccessProtocol::Git))
                        .unwrap();
            }
            let project = BTreeMap::from([("id", sea_orm::Value::Int(Some(7)))]);
            let mut db = MockDatabase::new(DatabaseBackend::Postgres);
            let (path, body) = match route {
                "assignment" => {
                    db = db
                        .append_query_results([vec![sandbox()]])
                        .append_query_results([vec![sandbox_service_assignments::Model {
                            id: "access_test".into(),
                            project_id: 7,
                            sandbox_id: 42,
                            service_id: service.id.clone(),
                            service_version: 1,
                            requested_by: Some(1),
                            approved_by: None,
                            approved_at: None,
                            expires_at: None,
                            revoked_at: None,
                            version: 1,
                            grant_id: None,
                            next_reconcile_at: Some(now),
                            created_at: now,
                            updated_at: now,
                        }]])
                        .append_query_results([vec![project]])
                        .append_query_results([vec![service]]);
                    (
                        "/projects/7/sandboxes/sbx_test/services/access_test/approve",
                        serde_json::json!({"expected_version":1,"service_version":requested_version}),
                    )
                }
                "grant" => {
                    db = db
                        .append_query_results([vec![project]])
                        .append_query_results([vec![service]]);
                    (
                        "/projects/7/sandboxes/sbx_test/service-grants/approve",
                        serde_json::json!({"request_id":uuid::Uuid::new_v4(),"service_id":"svc_test","service_version":requested_version,"ttl_seconds":60}),
                    )
                }
                _ => {
                    db = db
                        .append_query_results([vec![project]])
                        .append_query_results([vec![access_vaults::Model {
                            retired_at: None,
                            id: "vault_test".into(),
                            project_id: Some(7),
                            name: "Test vault".into(),
                            service_ids: vec!["svc_test".into()],
                            version: 1,
                            created_at: now,
                            updated_at: now,
                        }]])
                        .append_query_results([vec![service]]);
                    (
                        "/projects/7/access-vaults/vault_test/sessions",
                        serde_json::json!({"request_id":uuid::Uuid::new_v4(),"bundle_id":"bundle_test","bundle_version":1,"consumer":{"kind":"sandbox","sandbox_id":"sbx_test"},"ttl_seconds":60,"services":[{"service_id":"svc_test","version":requested_version}]}),
                    )
                }
            };
            let audit = Arc::new(Audit::default());
            let auth = AuthContext::new_api_key(
                test_user(),
                Some(Role::Admin),
                Some(
                    [
                        vec![Permission::ProjectsWrite, Permission::SandboxesWrite],
                        git_permissions,
                    ]
                    .concat(),
                ),
                "restricted-key".into(),
                1,
            );
            let response = send(
                "POST",
                path,
                &body.to_string(),
                Some(auth),
                state(db, audit.clone()),
            )
            .await;
            let status = response.status();
            let raw = to_bytes(response.into_body(), 8192).await.unwrap();
            assert_eq!(
                status,
                expected,
                "{route}: {}",
                String::from_utf8_lossy(&raw)
            );
            let problem: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            assert!(problem["title"]
                .as_str()
                .is_some_and(|title| !title.is_empty()));
            assert!(
                audit.0.lock().unwrap().is_empty(),
                "rejected authorization must not audit a successful approval"
            );
        }
    }
}

#[tokio::test]
async fn project_members_cannot_manage_general_vaults_or_links() {
    for (method, path, body) in routes().into_iter().filter(|(method, path, _)| {
        *method != "GET"
            && (path.contains("/access-services") || path.contains("/access-vaults"))
            && !path.ends_with("/sessions")
    }) {
        let response = send(
            method,
            path,
            body,
            Some(AuthContext::new_session(test_user(), Role::User)),
            state(
                MockDatabase::new(DatabaseBackend::Postgres),
                Arc::new(Audit::default()),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
}
