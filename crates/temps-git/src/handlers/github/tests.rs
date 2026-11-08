// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn installation_failures_map_to_fixed_codes() {
    assert_eq!(
        installation_failure_code(&GithubAppServiceError::InstallationNotFound),
        OperationFailureCode::NotFound
    );
    assert_eq!(
        installation_failure_code(&GithubAppServiceError::Unauthorized(
            "bad credentials".to_string()
        )),
        OperationFailureCode::Authentication
    );
    assert_eq!(
        installation_failure_code(&GithubAppServiceError::GithubApiError(
            "operation timed out".to_string()
        )),
        OperationFailureCode::Timeout
    );
}

#[test]
fn push_target_reads_branch_and_tag_refs() {
    assert_eq!(
        push_target("refs/heads/main", None),
        (Some("main".to_string()), None)
    );
    assert_eq!(
        push_target("refs/heads/feature/a-b", None),
        (Some("feature/a-b".to_string()), None)
    );
    assert_eq!(
        push_target("refs/tags/v1.2.0", Some("refs/heads/main")),
        (Some("main".to_string()), Some("v1.2.0".to_string()))
    );
    assert_eq!(
        push_target("refs/tags/v1.2.0", None),
        (None, Some("v1.2.0".to_string()))
    );
    assert_eq!(push_target("", None), (None, None));
}

fn detail_of(problem: &Problem) -> String {
    problem
        .body
        .get("detail")
        .and_then(|detail| detail.as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn manifest_conversion_returns_the_app_github_created() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/app-manifests/abc123/conversions")
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":42,"client_id":"client"}"#)
        .create_async()
        .await;

    let app = fetch_manifest_app(&server.url(), "abc123")
        .await
        .expect("a 201 reply is a created app");

    mock.assert_async().await;
    assert_eq!(app["id"], 42);
}

#[tokio::test]
async fn manifest_conversion_rejects_a_status_that_is_not_success() {
    for status in [404, 422, 500] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/app-manifests/not-a-code/conversions")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message":"Not Found"}"#)
            .create_async()
            .await;

        let problem = fetch_manifest_app(&server.url(), "not-a-code")
            .await
            .expect_err("an error status must not yield an app");

        mock.assert_async().await;
        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        assert!(
            detail_of(&problem).contains(&status.to_string()),
            "detail should name GitHub's status {status}: {}",
            detail_of(&problem)
        );
    }
}

#[tokio::test]
async fn manifest_code_is_percent_encoded_into_the_request_path() {
    let mut server = mockito::Server::new_async().await;
    // Without encoding, `/`, `?` and `#` would change which URL is requested.
    let mock = server
        .mock("POST", "/app-manifests/a%2Fb%3Fc%23d/conversions")
        .with_status(201)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":1}"#)
        .create_async()
        .await;

    fetch_manifest_app(&server.url(), "a/b?c#d")
        .await
        .expect("the encoded path should reach the mock");

    mock.assert_async().await;
}

#[tokio::test]
async fn manifest_conversion_errors_do_not_contain_the_code() {
    // Nothing listens on port 1, so the request fails before any reply.
    let problem = fetch_manifest_app("http://127.0.0.1:1", "one-time-code-value")
        .await
        .expect_err("the connection is refused");

    assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
    assert!(!detail_of(&problem).contains("one-time-code-value"));
}

#[test]
fn an_unusable_github_reply_is_a_502_that_names_the_missing_fields() {
    let problem = app_creation_problem(GithubAppServiceError::Validation(
        "GitHub's reply is missing a usable value for: pem".to_string(),
    ));

    assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
    assert!(detail_of(&problem).contains("pem"));
}

#[test]
fn an_app_that_is_already_registered_is_a_409() {
    let problem = app_creation_problem(GithubAppServiceError::Conflict(
        "GitHub App with id 7 already exists as provider 3".to_string(),
    ));

    assert_eq!(problem.status_code, StatusCode::CONFLICT);
    assert!(detail_of(&problem).contains("provider 3"));
}

#[test]
fn every_other_failure_stays_a_500() {
    for error in [
        GithubAppServiceError::DatabaseError(sea_orm::DbErr::Custom("db".to_string())),
        GithubAppServiceError::GithubApiError("api".to_string()),
        GithubAppServiceError::NotFound("app".to_string()),
        GithubAppServiceError::DecryptionFailed("key".to_string()),
        GithubAppServiceError::EncryptionFailed("key".to_string()),
        GithubAppServiceError::PrivateKeyCreationFailed("key".to_string()),
        GithubAppServiceError::InstallationNotFound,
        GithubAppServiceError::InvalidWebhookSignature,
        GithubAppServiceError::Unauthorized("no".to_string()),
        GithubAppServiceError::InvalidConfiguration("cfg".to_string()),
        GithubAppServiceError::Other("other".to_string()),
    ] {
        let description = format!("{error:?}");
        let problem = app_creation_problem(error);
        assert_eq!(
            problem.status_code,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{description}"
        );
    }
}

// ---- server-issued state on the manifest flow ----

fn pending_state(user_id: i32) -> temps_entities::github_manifest_states::Model {
    temps_entities::github_manifest_states::Model {
        id: 1,
        state_hash: "hash-of-issued-state".to_string(),
        user_id,
        expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
        created_at: chrono::Utc::now(),
    }
}

fn no_pending_state() -> sea_orm::MockDatabase {
    sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::github_manifest_states::Model>::new()])
}

#[tokio::test]
async fn a_code_without_a_state_the_server_issued_is_refused_before_github() {
    let state = super::super::public_route_tests::state_over(no_pending_state());

    // The state was not issued by this server. Had the code been sent to
    // GitHub the answer would be a 502, so a 400 shows it never was.
    let problem = handle_manifest_conversion_with_source(
        &state,
        "placeholder-manifest-code".to_string(),
        Some("state-the-server-did-not-issue".to_string()),
        None,
    )
    .await
    .expect_err("an unknown state is refused");

    assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_missing_or_empty_state_is_refused_without_touching_the_database() {
    // Nothing is queued on this database, so any query would fail with a 500.
    for manifest_state in [None, Some(String::new())] {
        let state = super::super::public_route_tests::state();
        let problem = handle_manifest_conversion_with_source(
            &state,
            "code".to_string(),
            manifest_state.clone(),
            None,
        )
        .await
        .expect_err("a request without a state is refused");

        assert_eq!(
            problem.status_code,
            StatusCode::BAD_REQUEST,
            "state {manifest_state:?}"
        );
    }
}

#[tokio::test]
async fn a_state_the_server_issued_identifies_the_operator() {
    let state = super::super::public_route_tests::state_over(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([[pending_state(7)]]),
    );

    let user_id = require_manifest_state(&state.git_provider_manager, Some("issued-state"))
        .await
        .expect("an issued state is accepted");

    assert_eq!(user_id, 7);
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

/// Keeps what was audited, as (operation type, serialized payload).
#[derive(Default)]
struct RecordingAuditLogger {
    operations: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl temps_core::AuditLogger for RecordingAuditLogger {
    async fn create_audit_log(
        &self,
        operation: &dyn temps_core::AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.operations
            .lock()
            .unwrap()
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

fn operator() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 7,
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

#[tokio::test]
async fn starting_the_flow_requires_authentication() {
    use tower::ServiceExt;

    let response = configure_routes()
        .with_state(super::super::public_route_tests::state())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/git-providers/github/manifest/start")
                .body(axum::body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_reader_cannot_start_the_flow_and_nothing_is_issued() {
    // Nothing is queued: issuing a state would be a database query and fail.
    let problem = start_github_manifest_flow(
        RequireAuth(temps_auth::AuthContext::new_session(
            operator(),
            temps_auth::Role::Reader,
        )),
        State(super::super::public_route_tests::state()),
        Extension(test_metadata()),
    )
    .await
    .err()
    .expect("a reader is refused");

    assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_operator_who_can_add_providers_gets_a_state_that_is_never_cached() {
    let state = super::super::public_route_tests::state_over(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([[pending_state(7)]]),
    );

    let (headers, body) = start_github_manifest_flow(
        RequireAuth(temps_auth::AuthContext::new_session(
            operator(),
            temps_auth::Role::Admin,
        )),
        State(state),
        Extension(test_metadata()),
    )
    .await
    .expect("an admin starts the flow");

    assert!(!body.0.state.is_empty());
    assert_eq!(
        headers.get("Cache-Control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}

#[tokio::test]
async fn starting_the_flow_is_audited_without_the_state() {
    let recorder = Arc::new(RecordingAuditLogger::default());
    let mut state = (*super::super::public_route_tests::state_over(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([[pending_state(7)]]),
    ))
    .clone();
    state.audit_service = recorder.clone();

    let (_, body) = start_github_manifest_flow(
        RequireAuth(temps_auth::AuthContext::new_session(
            operator(),
            temps_auth::Role::Admin,
        )),
        State(Arc::new(state)),
        Extension(test_metadata()),
    )
    .await
    .expect("an admin starts the flow");

    let recorded = recorder.operations.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].0, "GITHUB_APP_SETUP_STARTED");
    assert!(recorded[0].1.contains("\"user_id\":7"), "{}", recorded[0].1);
    assert!(recorded[0].1.contains("127.0.0.1"), "{}", recorded[0].1);
    assert!(
        !recorded[0].1.contains(&body.0.state),
        "the state is a credential and must not be audited"
    );
}

fn created_app() -> crate::services::github::GitHubAppData {
    crate::services::github::GitHubAppData {
        provider_id: 12,
        id: 12,
        app_id: 3456,
        name: "temps-example".to_string(),
        slug: "temps-example".to_string(),
        client_id: "Iv1.example-client".to_string(),
        client_secret: "example-client-secret-value".to_string(),
        private_key: "-----BEGIN RSA PRIVATE KEY-----example".to_string(),
        webhook_secret: "example-webhook-secret-value".to_string(),
        url: "https://github.com/apps/temps-example".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn creating_the_app_is_audited_for_the_operator_without_its_credentials() {
    let recorder = Arc::new(RecordingAuditLogger::default());
    let mut state = (*super::super::public_route_tests::state()).clone();
    state.audit_service = recorder.clone();
    let state = Arc::new(state);
    let app = created_app();

    record_github_app_created(&state, 7, Some(&test_metadata()), &app).await;

    let recorded = recorder.operations.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let (operation, payload) = &recorded[0];
    assert_eq!(operation, "GITHUB_APP_CREATED");
    for expected in [
        "\"user_id\":7",
        "\"provider_id\":12",
        "\"app_id\":3456",
        "\"app_name\":\"temps-example\"",
        "127.0.0.1",
    ] {
        assert!(payload.contains(expected), "missing {expected}: {payload}");
    }
    for secret in [&app.client_secret, &app.private_key, &app.webhook_secret] {
        assert!(
            !payload.contains(secret.as_str()),
            "a credential of the app was audited: {payload}"
        );
    }
}

#[tokio::test]
async fn creating_the_app_is_audited_even_without_request_metadata() {
    let recorder = Arc::new(RecordingAuditLogger::default());
    let mut state = (*super::super::public_route_tests::state()).clone();
    state.audit_service = recorder.clone();
    let state = Arc::new(state);

    record_github_app_created(&state, 7, None, &created_app()).await;

    let recorded = recorder.operations.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].0, "GITHUB_APP_CREATED");
    assert!(recorded[0].1.contains("\"user_id\":7"), "{}", recorded[0].1);
}

#[test]
fn the_start_endpoint_is_in_the_openapi_document() {
    use utoipa::OpenApi;
    let document = super::super::base::GitProvidersApiDoc::openapi();
    assert!(document
        .paths
        .paths
        .contains_key("/git-providers/github/manifest/start"));
}

// Delivery tests for the webhook signing-App binding (added on main).
/// Deliver a push signed by App 10, whose secret App 20 shares when
/// `shared` is set, for an installation owned by `provider_id`.
async fn signed_installation_delivery(provider_id: i32, shared: bool) -> (bool, usize) {
    use hmac::{Hmac, KeyInit, Mac};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::{git_provider_connections, git_providers, projects};
    let now = chrono::Utc::now();
    let app = git_providers::Model {
        id: 10,
        name: "example-app".into(),
        provider_type: "github".into(),
        base_url: None,
        api_url: None,
        auth_method: "app".into(),
        auth_config: serde_json::json!({
            "app_id": 100, "client_id": "example", "client_secret": "example",
            "private_key": "unused", "webhook_secret": "test-signing-secret"
        }),
        webhook_secret: None,
        is_active: true,
        is_default: false,
        created_at: now,
        updated_at: now,
    };
    let mut other_app = app.clone();
    other_app.id = 20;
    other_app.auth_config["app_id"] = serde_json::json!(101);
    if !shared {
        other_app.auth_config["webhook_secret"] = serde_json::json!("other-test-secret");
    }
    let connection: git_provider_connections::Model = serde_json::from_value(serde_json::json!({
        "id": 30, "provider_id": provider_id, "account_name": "example",
        "account_type": "Organization", "installation_id": "200",
        "is_active": true, "is_expired": false, "syncing": false,
        "synced_repository_count": 0, "health_status": "unknown",
        "consecutive_health_failures": 0, "created_at": now, "updated_at": now
    }))
    .unwrap();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![other_app, app]])
            .append_query_results([vec![connection.clone()], vec![connection]])
            .append_query_results([Vec::<projects::Model>::new()])
            .into_connection(),
    );
    let state = super::super::public_route_tests::state_with_db(db.clone());
    let body = serde_json::to_vec(&serde_json::json!({
        "installation": {"id": 200, "node_id": "example-installation"},
        "repository": {"id": 1, "name": "demo", "owner": {
            "login": "example", "id": 1, "node_id": "example",
            "avatar_url": "https://example.com/avatar", "gravatar_id": "",
            "url": "https://example.com/user", "html_url": "https://example.com/user",
            "followers_url": "https://example.com/followers",
            "following_url": "https://example.com/following",
            "gists_url": "https://example.com/gists", "starred_url": "https://example.com/starred",
            "subscriptions_url": "https://example.com/subscriptions",
            "organizations_url": "https://example.com/orgs",
            "repos_url": "https://example.com/repos", "events_url": "https://example.com/events",
            "received_events_url": "https://example.com/received", "type": "Organization",
            "site_admin": false
        }, "url": "https://example.com/repo"},
        "ref": "refs/heads/topic", "after": "0000000000000000000000000000000000000000",
        "before": "1111111111111111111111111111111111111111",
        "commits": [], "compare": "https://example.com/compare",
        "created": false, "deleted": true, "forced": false,
        "pusher": {"name": "example", "email": "example@example.com"}
    }))
    .unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"test-signing-secret").unwrap();
    mac.update(&body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    let mut headers = HeaderMap::new();
    headers.insert("X-Hub-Signature-256", signature.parse().unwrap());
    headers.insert("X-GitHub-Event", "push".parse().unwrap());
    let result = github_webhook_events(headers, State(state.clone()), Bytes::from(body)).await;
    let owned = provider_id == 10 || (shared && provider_id == 20);
    if !owned {
        assert_eq!(
            result.as_ref().err().unwrap().status_code,
            StatusCode::UNAUTHORIZED
        );
    } else {
        assert!(result.is_ok(), "{:?}", result.as_ref().err());
    }
    drop(state);
    let queries = Arc::try_unwrap(db).unwrap().into_transaction_log().len();
    (result.is_ok(), queries)
}

#[tokio::test]
async fn signing_app_mismatch_dispatches_nothing() {
    // Only signature and ownership queries run; no dispatch queries or jobs.
    assert_eq!(signed_installation_delivery(20, false).await, (false, 2));
}

#[tokio::test]
async fn shared_secret_binds_the_app_that_owns_the_installation() {
    // App 20 is checked first and shares the secret, but App 10 owns it.
    assert_eq!(signed_installation_delivery(10, true).await, (true, 4));
}

#[tokio::test]
async fn shared_secret_still_rejects_an_installation_no_signer_owns() {
    assert_eq!(signed_installation_delivery(30, true).await, (false, 2));
}

#[tokio::test]
async fn signing_app_pairing_dispatches_branch_deletion() {
    // The matching App reaches the installation and project dispatch queries.
    assert_eq!(signed_installation_delivery(10, false).await, (true, 4));
}

/// Collects the fields of every tracing event, so a test can read what was
/// logged.
struct CaptureLogs(Arc<std::sync::Mutex<Vec<String>>>);

struct EventText(String);

impl tracing::field::Visit for EventText {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={value:?} ", field.name()));
    }
}

impl tracing::Subscriber for CaptureLogs {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut text = EventText(String::new());
        event.record(&mut text);
        self.0.lock().unwrap().push(text.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn the_callback_never_logs_the_state_or_the_code() {
    let secret_state = "state-that-must-stay-out-of-the-logs";
    let secret_code = "code-that-must-stay-out-of-the-logs";
    let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let guard = tracing::subscriber::set_default(CaptureLogs(logs.clone()));

    // The callback reads the operator's external URL first, then looks the
    // state up: queue one settings row and no pending state.
    let settings = temps_entities::settings::Model {
        id: 1,
        data: temps_config::AppSettings {
            external_url: Some("https://temps.example.test".to_string()),
            ..temps_config::AppSettings::default()
        }
        .to_json(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let state = super::super::public_route_tests::state_over(
        sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![settings]])
            .append_query_results([Vec::<temps_entities::github_manifest_states::Model>::new()]),
    );
    let params = HashMap::from([
        ("code".to_string(), secret_code.to_string()),
        ("state".to_string(), secret_state.to_string()),
    ]);
    let result = github_app_auth_callback(State(state), None, Query(params)).await;
    drop(guard);

    assert_eq!(
        result.expect_err("an unknown state is refused").status_code,
        StatusCode::BAD_REQUEST
    );
    let logged = logs.lock().unwrap().join("\n");
    assert!(
        logged.contains("Detected manifest conversion flow"),
        "the test sees the callback's log line: {logged}"
    );
    assert!(
        !logged.contains(secret_state),
        "the state was logged: {logged}"
    );
    assert!(
        !logged.contains(secret_code),
        "the code was logged: {logged}"
    );
}
