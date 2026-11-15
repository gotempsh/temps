// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Refresh behaviour against real Postgres, whose compare-and-set semantics are
//! the point, and a local token endpoint standing in for the provider. Each test
//! skips with a message when no test database can be started.

use super::super::oauth::OAuthCredentials;
use super::super::test_support::*;
use super::*;
use crate::services::connection_health::{
    ConnectionHealthService, HEALTH_STATUS_HEALTHY, HEALTH_STATUS_UNHEALTHY,
};
use crate::services::git_provider_manager_trait::GitProviderManagerTrait;
use crate::services::gitlab_provider::GitLabProvider;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use temps_test_utils::TestDatabase;
use tokio::sync::{oneshot, Notify, Semaphore};

const CONNECTION_ID: i32 = 7001;
const STORED_ACCESS: &str = "stored-access";
const STORED_REFRESH: &str = "stored-refresh";
/// In a response body, so a test can prove that it never reaches an error.
const BODY_MARKER: &str = "sensitive-upstream-marker";

fn issued_body() -> String {
    r#"{"access_token":"issued-access","refresh_token":"issued-refresh","expires_in":7200}"#
        .to_string()
}

fn rejected_body() -> String {
    format!(r#"{{"error":"invalid_grant","error_description":"{BODY_MARKER}"}}"#)
}

/// The provider's side: counts requests and answers from a script, optionally
/// holding the refresh open so a test can change the row while it is in flight.
struct Endpoint {
    refresh_calls: AtomicUsize,
    user_calls: AtomicUsize,
    refresh_response: Mutex<(u16, String)>,
    user_status: Mutex<u16>,
    /// When set, a token check waits here after being counted, so a test can
    /// order what happens between the caller's read and its write.
    user_gate: Mutex<Option<Arc<Semaphore>>>,
    hold: Mutex<Option<oneshot::Receiver<()>>>,
    refresh_reached: Notify,
    request_bodies: Mutex<Vec<String>>,
}

async fn refresh_route(
    State(endpoint): State<Arc<Endpoint>>,
    body: String,
) -> axum::response::Response {
    endpoint.refresh_calls.fetch_add(1, Ordering::SeqCst);
    endpoint.request_bodies.lock().unwrap().push(body);
    endpoint.refresh_reached.notify_one();
    let hold = endpoint.hold.lock().unwrap().take();
    if let Some(release) = hold {
        let _ = release.await;
    }
    let (status, body) = endpoint.refresh_response.lock().unwrap().clone();
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        body,
    )
        .into_response()
}

async fn user_route(State(endpoint): State<Arc<Endpoint>>) -> axum::response::Response {
    endpoint.user_calls.fetch_add(1, Ordering::SeqCst);
    let gate = endpoint.user_gate.lock().unwrap().clone();
    if let Some(gate) = gate {
        gate.acquire().await.unwrap().forget();
    }
    let status = *endpoint.user_status.lock().unwrap();
    (StatusCode::from_u16(status).unwrap(), r#"{"id":1}"#).into_response()
}

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
        self.operations
            .lock()
            .unwrap()
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

struct Harness {
    // Held so the schema outlives the test.
    _database: TestDatabase,
    db: Arc<DatabaseConnection>,
    manager: GitProviderManager,
    endpoint: Arc<Endpoint>,
    audit: Arc<RecordingAudit>,
    base_url: String,
}

impl Harness {
    /// A GitLab OAuth connection whose recorded expiry has passed, so the next
    /// use refreshes. `None` when no test database can be started.
    async fn start() -> Option<Self> {
        let database = match TestDatabase::with_migrations().await {
            Ok(database) => database,
            Err(error) => {
                eprintln!("Skipping token refresh test: no test database: {error}");
                return None;
            }
        };
        let db = database.connection_arc();

        let endpoint = Arc::new(Endpoint {
            refresh_calls: AtomicUsize::new(0),
            user_calls: AtomicUsize::new(0),
            refresh_response: Mutex::new((200, issued_body())),
            user_status: Mutex::new(200),
            user_gate: Mutex::new(None),
            hold: Mutex::new(None),
            refresh_reached: Notify::new(),
            request_bodies: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/oauth/token", post(refresh_route))
            .route("/api/v4/user", get(user_route))
            .route("/user", get(user_route))
            .with_state(endpoint.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let audit = Arc::new(RecordingAudit::default());
        let manager = Self::manager_for(&db, &base_url, audit.clone()).await;

        db.execute_unprepared(&format!(
            "INSERT INTO users (id, name, email, created_at, updated_at)
                VALUES (7001, 'Example user', 'refresh@example.test', now(), now());
             INSERT INTO git_providers (id, name, provider_type, base_url, api_url, auth_method, auth_config, is_active, is_default, created_at, updated_at)
                VALUES (7001, 'Example GitLab', 'gitlab', '{base_url}', '{base_url}', 'oauth',
                        '{{\"client_id\":\"example-client\",\"client_secret\":\"example-secret\",\"redirect_uri\":\"https://console.example.test/callback\"}}',
                        true, false, now(), now());
             INSERT INTO git_provider_connections (id, provider_id, user_id, account_name, account_type, is_active, is_expired, metadata, created_at, updated_at)
                VALUES ({CONNECTION_ID}, 7001, 7001, 'example-account', 'User', true, false, '{{\"id\":123}}', now(), now())"
        ))
        .await
        .unwrap();

        let harness = Self {
            _database: database,
            db,
            manager,
            endpoint,
            audit,
            base_url,
        };
        let mut row = harness.row().await.into_active_model();
        row.access_token = Set(Some(harness.encrypt(STORED_ACCESS)));
        row.refresh_token = Set(Some(harness.encrypt(STORED_REFRESH)));
        row.token_expires_at = Set(Some(Utc::now() - chrono::Duration::minutes(1)));
        row.update(harness.db.as_ref()).await.unwrap();
        Some(harness)
    }

    /// A manager with its own caches, as a second process would have.
    async fn manager_for(
        db: &Arc<DatabaseConnection>,
        base_url: &str,
        audit: Arc<RecordingAudit>,
    ) -> GitProviderManager {
        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new(KEY).unwrap()),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db.clone()),
            audit,
        );
        // Injected so the provider talks to the local endpoint.
        let provider: Arc<dyn GitProviderService> = Arc::new(GitLabProvider::new(
            Some(base_url.to_string()),
            AuthMethod::OAuth {
                client_id: "example-client".to_string(),
                client_secret: "example-secret".to_string(),
                redirect_uri: "https://console.example.test/callback".to_string(),
            },
        ));
        manager.providers_cache.write().await.insert(7001, provider);
        manager
    }

    fn encrypt(&self, plaintext: &str) -> String {
        self.manager
            .encryption_service
            .encrypt_string(plaintext)
            .unwrap()
    }

    fn decrypt(&self, ciphertext: Option<&str>) -> String {
        self.manager
            .encryption_service
            .decrypt_string(ciphertext.expect("token stored"))
            .unwrap()
    }

    async fn row(&self) -> git_provider_connections::Model {
        self.manager.get_connection(CONNECTION_ID).await.unwrap()
    }

    fn respond(&self, status: u16, body: String) {
        *self.endpoint.refresh_response.lock().unwrap() = (status, body);
    }

    /// Hold the next refresh open until the returned sender fires.
    fn hold_refresh(&self) -> oneshot::Sender<()> {
        let (release, held) = oneshot::channel();
        *self.endpoint.hold.lock().unwrap() = Some(held);
        release
    }

    fn gate_user_route(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.endpoint.user_gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    async fn wait_for_user_calls(&self, calls: usize) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.user_calls() < calls {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    fn refresh_calls(&self) -> usize {
        self.endpoint.refresh_calls.load(Ordering::SeqCst)
    }

    fn user_calls(&self) -> usize {
        self.endpoint.user_calls.load(Ordering::SeqCst)
    }

    fn audited(&self) -> Vec<(String, String)> {
        self.audit.operations.lock().unwrap().clone()
    }

    async fn block(&self) {
        let mut row = self.row().await.into_active_model();
        row.is_expired = Set(true);
        row.update(self.db.as_ref()).await.unwrap();
    }

    /// A user finishing reauthorization, through the real write path.
    async fn reauthorize(&self) {
        let row = self.row().await;
        self.manager
            .replace_oauth_grant(
                row,
                OAuthCredentials {
                    account_name: "example-account".to_string(),
                    account_type: "User".to_string(),
                    access_token: "reauthorized-access".to_string(),
                    refresh_token: Some("reauthorized-refresh".to_string()),
                    expires_at: Some(Utc::now() + chrono::Duration::hours(2)),
                    metadata: serde_json::json!({"id": 123}),
                },
            )
            .await
            .unwrap();
    }

    /// Another process's refresh landing: new tokens in the same generation.
    async fn rotate(&self) {
        let mut row = self.row().await.into_active_model();
        row.access_token = Set(Some(self.encrypt("rotated-access")));
        row.refresh_token = Set(Some(self.encrypt("rotated-refresh")));
        row.update(self.db.as_ref()).await.unwrap();
    }

    fn health_service(&self) -> ConnectionHealthService {
        let manager = Arc::new(self.manager.clone());
        ConnectionHealthService::new(
            self.db.clone(),
            manager.clone(),
            Arc::new(crate::services::github::GithubAppService::new(
                self.db.clone(),
                Arc::new(MockJobQueue),
                manager,
            )),
            "https://console.example.test".to_string(),
        )
    }
}

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn assert_says_reauthorize(message: &str) {
    assert!(
        message.contains(&CONNECTION_ID.to_string()),
        "must name the connection: {message}"
    );
    assert!(
        message.contains("Reauthorize"),
        "must say what to do: {message}"
    );
    for leak in [BODY_MARKER, STORED_ACCESS, STORED_REFRESH] {
        assert!(!message.contains(leak), "leaked {leak}: {message}");
    }
}

#[tokio::test]
async fn a_rejected_refresh_blocks_the_connection_and_audits_it_once() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.respond(400, rejected_body());

    let error = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap_err();

    assert!(
        matches!(
            error,
            GitProviderManagerError::ConnectionReauthorizationRequired {
                connection_id: CONNECTION_ID
            }
        ),
        "{error}"
    );
    assert_says_reauthorize(&error.to_string());
    let row = h.row().await;
    assert!(row.is_expired);
    assert_eq!(h.refresh_calls(), 1);
    assert_eq!(row.token_refresh_lease_id, None, "lease must be released");

    let audited = h.audited();
    assert_eq!(audited.len(), 1, "{audited:?}");
    assert_eq!(audited[0].0, "GIT_CONNECTION_REFRESH_BLOCKED");
    assert!(audited[0].1.contains(&CONNECTION_ID.to_string()));
    for leak in [STORED_ACCESS, STORED_REFRESH, BODY_MARKER] {
        assert!(!audited[0].1.contains(leak), "{audited:?}");
    }
}

#[tokio::test]
async fn failures_that_do_not_prove_a_dead_token_do_not_block() {
    let Some(h) = Harness::start().await else {
        return;
    };
    for (status, body) in [
        (500, r#"{"error":"invalid_grant"}"#.to_string()),
        (503, "unavailable".to_string()),
        (429, r#"{"error":"invalid_grant"}"#.to_string()),
        (401, r#"{"error":"invalid_client"}"#.to_string()),
        (400, r#"{"error":"invalid_client"}"#.to_string()),
    ] {
        h.respond(status, body.clone());
        let before = h.refresh_calls();

        let error = h
            .manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
            .unwrap_err();

        assert!(
            !matches!(
                error,
                GitProviderManagerError::ConnectionReauthorizationRequired { .. }
            ),
            "HTTP {status} {body} must not require reauthorization: {error}"
        );
        assert_eq!(h.refresh_calls(), before + 1, "HTTP {status} was retried");
        let row = h.row().await;
        assert!(
            !row.is_expired,
            "HTTP {status} {body} blocked the connection"
        );
        assert_eq!(h.decrypt(row.refresh_token.as_deref()), STORED_REFRESH);
    }
    assert!(h.audited().is_empty());
}

#[tokio::test]
async fn a_network_failure_does_not_block() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let manager = Harness::manager_for(&h.db, &dead, h.audit.clone()).await;

    assert!(manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .is_err());

    assert!(!h.row().await.is_expired);
}

#[tokio::test]
async fn a_transient_failure_inside_the_buffer_keeps_using_the_stored_token() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let mut row = h.row().await.into_active_model();
    row.token_expires_at = Set(Some(Utc::now() + chrono::Duration::minutes(2)));
    row.update(h.db.as_ref()).await.unwrap();
    h.respond(503, "unavailable".to_string());

    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(token, STORED_ACCESS);
    assert!(!h.row().await.is_expired);
}

#[tokio::test]
async fn a_rejection_of_an_already_rotated_token_reuses_the_new_one() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    h.respond(400, rejected_body());
    let manager = h.manager.clone();
    let caller = tokio::spawn(async move {
        manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
    });
    h.endpoint.refresh_reached.notified().await;

    h.rotate().await;
    release.send(()).unwrap();

    assert_eq!(caller.await.unwrap().unwrap(), "rotated-access");
    let row = h.row().await;
    assert!(!row.is_expired, "a rotated token must not block");
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), "rotated-refresh");
    assert!(h.audited().is_empty());
}

#[tokio::test]
async fn a_block_only_lands_while_the_stored_credentials_are_the_ones_sent() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let sent = h.row().await;
    let updated_at = sent.updated_at;

    // A reauthorization landing after the last read must not be marked dead.
    h.reauthorize().await;
    assert!(!h.manager.mark_refresh_blocked(&sent).await.unwrap());
    let row = h.row().await;
    assert!(!row.is_expired);
    assert_eq!(
        h.decrypt(row.access_token.as_deref()),
        "reauthorized-access"
    );

    // The credentials that are stored now do block, once, and without moving
    // the version an in-progress reauthorization compares against.
    let updated_at_after_reauthorization = row.updated_at;
    assert_ne!(updated_at, updated_at_after_reauthorization);
    assert!(h.manager.mark_refresh_blocked(&row).await.unwrap());
    let blocked = h.row().await;
    assert!(blocked.is_expired);
    assert_eq!(blocked.updated_at, updated_at_after_reauthorization);
    assert!(!h.manager.mark_refresh_blocked(&row).await.unwrap());
}

#[tokio::test]
async fn a_caller_with_a_stale_read_reuses_the_rotated_token_without_a_request() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let stale = h.row().await;
    h.rotate().await;

    let token = h
        .manager
        .refresh_connection_token(&stale, BlockedConnection::Fail)
        .await
        .unwrap();

    assert_eq!(token, "rotated-access");
    assert_eq!(h.refresh_calls(), 0);
}

#[tokio::test]
async fn a_reauthorization_during_a_refresh_is_never_overwritten() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    let manager = h.manager.clone();
    let caller = tokio::spawn(async move {
        manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
    });
    h.endpoint.refresh_reached.notified().await;

    h.reauthorize().await;
    release.send(()).unwrap();

    assert_eq!(
        caller.await.unwrap().unwrap(),
        "reauthorized-access",
        "the caller must continue with the grant that is stored"
    );
    let row = h.row().await;
    assert_eq!(
        h.decrypt(row.access_token.as_deref()),
        "reauthorized-access"
    );
    assert_eq!(
        h.decrypt(row.refresh_token.as_deref()),
        "reauthorized-refresh"
    );
    assert!(!row.is_expired);
}

#[tokio::test]
async fn a_reauthorization_during_a_rejected_refresh_is_not_blocked() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    h.respond(400, rejected_body());
    let manager = h.manager.clone();
    let caller = tokio::spawn(async move {
        manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
    });
    h.endpoint.refresh_reached.notified().await;

    h.reauthorize().await;
    release.send(()).unwrap();

    assert_eq!(caller.await.unwrap().unwrap(), "reauthorized-access");
    let row = h.row().await;
    assert!(
        !row.is_expired,
        "a completed reauthorization must not be blocked"
    );
    assert_eq!(
        h.decrypt(row.refresh_token.as_deref()),
        "reauthorized-refresh"
    );
    assert!(h.audited().is_empty());
}

#[tokio::test]
async fn tokens_issued_while_another_refresh_rotated_the_row_are_kept() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    let manager = h.manager.clone();
    let caller = tokio::spawn(async move {
        manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
    });
    h.endpoint.refresh_reached.notified().await;

    // The generation is unchanged, so this is a rotation and not a new grant.
    h.rotate().await;
    release.send(()).unwrap();

    assert_eq!(caller.await.unwrap().unwrap(), "issued-access");
    let row = h.row().await;
    assert_eq!(h.decrypt(row.access_token.as_deref()), "issued-access");
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), "issued-refresh");
}

#[tokio::test]
async fn concurrent_callers_make_one_refresh_call_and_share_its_result() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    // A second manager has separate in-memory state, so only the lease in the
    // database can make the two agree, as it must between processes.
    let other_process = Harness::manager_for(&h.db, &h.base_url, h.audit.clone()).await;

    let mut callers = Vec::new();
    for index in 0..8 {
        let manager = if index % 2 == 0 {
            h.manager.clone()
        } else {
            other_process.clone()
        };
        callers.push(tokio::spawn(async move {
            manager
                .validate_and_refresh_connection_token(CONNECTION_ID)
                .await
        }));
    }
    h.endpoint.refresh_reached.notified().await;
    // Give every other caller time to find the lease taken.
    tokio::time::sleep(Duration::from_millis(500)).await;
    release.send(()).unwrap();

    for caller in callers {
        assert_eq!(caller.await.unwrap().unwrap(), "issued-access");
    }
    assert_eq!(h.refresh_calls(), 1, "refresh tokens may be single-use");
    let row = h.row().await;
    assert_eq!(row.token_refresh_lease_id, None);
    assert_eq!(row.token_refresh_lease_expires_at, None);
}

#[tokio::test]
async fn callers_waiting_on_a_failed_refresh_do_not_repeat_the_request() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    h.respond(503, "unavailable".to_string());

    let mut callers = Vec::new();
    for _ in 0..4 {
        let manager = h.manager.clone();
        callers.push(tokio::spawn(async move {
            manager
                .validate_and_refresh_connection_token(CONNECTION_ID)
                .await
        }));
    }
    h.endpoint.refresh_reached.notified().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    release.send(()).unwrap();

    for caller in callers {
        assert!(caller.await.unwrap().is_err());
    }
    assert_eq!(h.refresh_calls(), 1);
    assert!(!h.row().await.is_expired);
}

#[tokio::test]
async fn a_lapsed_lease_from_a_crashed_holder_does_not_block_later_refreshes() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let mut row = h.row().await.into_active_model();
    row.token_refresh_lease_id = Set(Some(uuid::Uuid::new_v4()));
    row.token_refresh_lease_expires_at = Set(Some(Utc::now() + chrono::Duration::seconds(1)));
    row.update(h.db.as_ref()).await.unwrap();

    // While the lease is live a caller waits, then reports it rather than
    // making the request itself.
    let error = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            GitProviderManagerError::TokenRefreshIncomplete {
                connection_id: CONNECTION_ID
            }
        ),
        "{error}"
    );
    assert_eq!(h.refresh_calls(), 0);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();
    assert_eq!(token, "issued-access");
    assert_eq!(h.refresh_calls(), 1);
}

#[tokio::test]
async fn cancelling_the_caller_mid_refresh_does_not_lose_the_issued_tokens() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let release = h.hold_refresh();
    let manager = h.manager.clone();
    let caller = tokio::spawn(async move {
        manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
    });
    h.endpoint.refresh_reached.notified().await;

    caller.abort();
    let _ = caller.await;
    release.send(()).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row = h.row().await;
        if row.token_refresh_lease_id.is_none() {
            assert_eq!(h.decrypt(row.refresh_token.as_deref()), "issued-refresh");
            assert_eq!(h.decrypt(row.access_token.as_deref()), "issued-access");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the detached refresh never finished"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn syncs_and_deploys_fail_on_a_blocked_connection_without_calling_the_provider() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.block().await;
    let directory = tempfile::tempdir().unwrap();

    let mut messages = vec![
        h.manager
            .get_connection_token(CONNECTION_ID)
            .await
            .unwrap_err()
            .to_string(),
        h.manager
            .validate_and_refresh_connection_token(CONNECTION_ID)
            .await
            .unwrap_err()
            .to_string(),
        h.manager
            .force_refresh_connection_token(CONNECTION_ID)
            .await
            .unwrap_err()
            .to_string(),
        h.manager
            .execute_with_refresh(CONNECTION_ID, |_token| async {
                Ok::<(), GitProviderError>(())
            })
            .await
            .unwrap_err()
            .to_string(),
        h.manager
            .sync_repositories(CONNECTION_ID)
            .await
            .unwrap_err()
            .to_string(),
    ];
    let deploy = GitProviderManagerTrait::get_connection_access_token(&h.manager, CONNECTION_ID)
        .await
        .unwrap_err();
    assert!(matches!(
        deploy,
        crate::services::git_provider_manager_trait::GitProviderManagerError::ReauthorizationRequired {
            connection_id: CONNECTION_ID
        }
    ));
    messages.push(deploy.to_string());
    messages.push(
        GitProviderManagerTrait::clone_repository(
            &h.manager,
            CONNECTION_ID,
            "example-owner",
            "example-repository",
            &directory.path().join("clone"),
            Some("main"),
        )
        .await
        .unwrap_err()
        .to_string(),
    );
    messages.push(
        GitProviderManagerTrait::download_archive(
            &h.manager,
            CONNECTION_ID,
            "example-owner",
            "example-repository",
            "main",
            &directory.path().join("archive.tar.gz"),
            None,
        )
        .await
        .unwrap_err()
        .to_string(),
    );

    for message in &messages {
        assert_says_reauthorize(message);
    }
    assert_eq!(h.refresh_calls(), 0);
    assert_eq!(h.user_calls(), 0);
    assert!(h.row().await.is_expired);
}

#[tokio::test]
async fn a_manual_health_check_tries_once_and_clears_the_block_on_success() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.block().await;

    let outcome = h
        .health_service()
        .check_connection_health(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(outcome.status, HEALTH_STATUS_HEALTHY);
    assert_eq!(h.refresh_calls(), 1);
    let row = h.row().await;
    assert!(!row.is_expired, "a successful check must clear the block");
    assert_eq!(row.health_status, HEALTH_STATUS_HEALTHY);
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), "issued-refresh");
    let audited = h.audited();
    assert_eq!(audited.len(), 1, "{audited:?}");
    assert_eq!(audited[0].0, "GIT_CONNECTION_REFRESH_UNBLOCKED");

    // The block is gone, so syncs work again.
    assert!(h.manager.get_connection_token(CONNECTION_ID).await.is_ok());
}

#[tokio::test]
async fn a_health_check_still_rejected_stays_blocked_and_shows_no_provider_response() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.block().await;
    h.respond(400, rejected_body());

    let outcome = h
        .health_service()
        .check_connection_health(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(outcome.status, HEALTH_STATUS_UNHEALTHY);
    assert_eq!(h.refresh_calls(), 1, "a check tries exactly once");
    let row = h.row().await;
    assert!(row.is_expired);
    assert_eq!(
        row.health_message.as_deref(),
        Some("GitLab no longer accepts this connection's authorization")
    );
    assert!(
        h.audited().is_empty(),
        "already blocked, so nothing transitioned"
    );
}

#[tokio::test]
async fn a_health_check_refreshes_a_token_the_recorded_expiry_wrongly_calls_valid() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let mut row = h.row().await.into_active_model();
    row.token_expires_at = Set(Some(Utc::now() + chrono::Duration::hours(1)));
    row.update(h.db.as_ref()).await.unwrap();
    *h.endpoint.user_status.lock().unwrap() = 401;
    h.respond(400, rejected_body());

    let outcome = h
        .health_service()
        .check_connection_health(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(outcome.status, HEALTH_STATUS_UNHEALTHY);
    assert_eq!(
        h.refresh_calls(),
        1,
        "must try the refresh, not just report the rejected access token"
    );
    assert!(h.row().await.is_expired);
}

#[tokio::test]
async fn a_health_check_during_an_outage_leaves_the_connection_unblocked() {
    let Some(h) = Harness::start().await else {
        return;
    };
    *h.endpoint.user_status.lock().unwrap() = 503;

    let outcome = h
        .health_service()
        .check_connection_health(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(outcome.status, HEALTH_STATUS_UNHEALTHY);
    assert_eq!(
        h.refresh_calls(),
        0,
        "an outage says nothing about the token"
    );
    assert!(!h.row().await.is_expired);
}

#[tokio::test]
async fn a_refresh_stores_the_expiry_the_provider_reported() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let before = Utc::now();

    h.manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    let expires_at = h.row().await.token_expires_at.expect("expiry recorded");
    assert!(
        expires_at >= before + chrono::Duration::seconds(7199),
        "expected about 2h from expires_in, got {expires_at}"
    );
    assert!(expires_at <= Utc::now() + chrono::Duration::seconds(7200));
}

#[tokio::test]
async fn a_token_that_still_validates_keeps_its_recorded_expiry() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let recorded = (Utc::now() + chrono::Duration::minutes(20)).with_timezone(&Utc);
    let mut row = h.row().await.into_active_model();
    row.token_expires_at = Set(Some(recorded));
    let recorded = row.update(h.db.as_ref()).await.unwrap().token_expires_at;

    // Neither the ordinary lookup nor the post-auth-failure path may touch it.
    assert_eq!(
        h.manager.get_connection_token(CONNECTION_ID).await.unwrap(),
        STORED_ACCESS
    );
    assert_eq!(
        h.manager
            .force_refresh_connection_token(CONNECTION_ID)
            .await
            .unwrap(),
        STORED_ACCESS
    );

    let row = h.row().await;
    assert_eq!(row.token_expires_at, recorded);
    assert_eq!(h.refresh_calls(), 0);
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), STORED_REFRESH);
}

#[tokio::test]
async fn a_missing_lifetime_is_recorded_as_unknown_rather_than_guessed() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.respond(200, r#"{"access_token":"issued-access"}"#.to_string());

    h.manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    let row = h.row().await;
    assert_eq!(row.token_expires_at, None);
    assert_eq!(
        h.decrypt(row.refresh_token.as_deref()),
        STORED_REFRESH,
        "a provider that does not rotate leaves the refresh token in place"
    );
}

/// Leaves the connection as a provider that reports no `expires_in` does.
async fn forget_expiry(h: &Harness) {
    let mut row = h.row().await.into_active_model();
    row.token_expires_at = Set(None);
    row.update(h.db.as_ref()).await.unwrap();
}

#[tokio::test]
async fn a_token_of_unknown_lifetime_is_refreshed_once_the_provider_rejects_it() {
    let Some(h) = Harness::start().await else {
        return;
    };
    forget_expiry(&h).await;
    *h.endpoint.user_status.lock().unwrap() = 401;
    h.respond(200, issued_body());

    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(token, "issued-access");
    assert_eq!(h.refresh_calls(), 1);
    assert_eq!(h.user_calls(), 1);
    let row = h.row().await;
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), "issued-refresh");
    assert!(row.token_expires_at.is_some());
}

#[tokio::test]
async fn a_token_of_unknown_lifetime_the_provider_accepts_spends_no_refresh_token() {
    let Some(h) = Harness::start().await else {
        return;
    };
    forget_expiry(&h).await;

    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(token, STORED_ACCESS);
    assert_eq!(h.user_calls(), 1);
    assert_eq!(h.refresh_calls(), 0);
    assert_eq!(h.row().await.token_expires_at, None);
}

#[tokio::test]
async fn an_outage_does_not_spend_the_refresh_token_of_an_unknown_lifetime() {
    let Some(h) = Harness::start().await else {
        return;
    };
    forget_expiry(&h).await;
    *h.endpoint.user_status.lock().unwrap() = 503;

    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(token, STORED_ACCESS);
    assert_eq!(h.refresh_calls(), 0);
    let row = h.row().await;
    assert!(!row.is_expired);
    assert_eq!(h.decrypt(row.refresh_token.as_deref()), STORED_REFRESH);
}

#[tokio::test]
async fn a_token_of_unknown_lifetime_without_a_refresh_token_is_not_checked() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let mut row = h.row().await.into_active_model();
    row.token_expires_at = Set(None);
    row.refresh_token = Set(None);
    row.update(h.db.as_ref()).await.unwrap();
    *h.endpoint.user_status.lock().unwrap() = 401;

    let token = h
        .manager
        .validate_and_refresh_connection_token(CONNECTION_ID)
        .await
        .unwrap();

    assert_eq!(token, STORED_ACCESS);
    assert_eq!(
        h.user_calls(),
        0,
        "without a refresh token there is no way out"
    );
    assert_eq!(h.refresh_calls(), 0);
}

#[tokio::test]
async fn the_refresh_request_carries_the_stored_token_and_no_second_attempt() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.respond(500, "unavailable".to_string());

    assert!(h
        .manager
        .force_refresh_connection_token(CONNECTION_ID)
        .await
        .is_ok());
    assert_eq!(h.refresh_calls(), 0, "the access token still validates");

    *h.endpoint.user_status.lock().unwrap() = 401;
    assert!(h
        .manager
        .force_refresh_connection_token(CONNECTION_ID)
        .await
        .is_err());

    assert_eq!(h.refresh_calls(), 1);
    let bodies = h.endpoint.request_bodies.lock().unwrap().clone();
    assert!(bodies[0].contains(&format!("refresh_token={STORED_REFRESH}")));
    assert!(bodies[0].contains("grant_type=refresh_token"));
}

#[tokio::test]
async fn replacing_the_token_through_update_token_bumps_the_generation() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let before = h.row().await;

    h.manager
        .update_connection_token(CONNECTION_ID, "replacement-access".to_string(), None)
        .await
        .unwrap();

    let after = h.row().await;
    assert_eq!(
        after.credential_generation,
        before.credential_generation + 1
    );
    assert!(matches!(
        h.manager
            .update_connection_tokens(&before, "stale-access".to_string(), None, None)
            .await,
        Err(GitProviderManagerError::ConnectionGrantReplaced {
            connection_id: CONNECTION_ID
        })
    ));
    assert_eq!(
        h.decrypt(h.row().await.access_token.as_deref()),
        "replacement-access"
    );
}

/// Both updates read the row before their validation request, so they start from
/// the same generation. The second must still move it past the one a refresh
/// picked up after the first, or that refresh would overwrite the second token.
#[tokio::test]
async fn token_updates_from_the_same_read_each_move_the_generation() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let before = h.row().await.credential_generation;
    let gate = h.gate_user_route();

    let update = |token: &'static str| {
        h.manager
            .update_connection_token(CONNECTION_ID, token.to_string(), None)
    };
    let (first, second, refresh_started_from) =
        tokio::join!(update("first-access"), update("second-access"), async {
            h.wait_for_user_calls(2).await;
            gate.add_permits(1);
            let row = loop {
                let row = h.row().await;
                if row.credential_generation != before {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            gate.add_permits(1);
            row
        });
    first.unwrap();
    second.unwrap();

    assert_eq!(h.row().await.credential_generation, before + 2);
    let token_after_first = h.decrypt(refresh_started_from.access_token.as_deref());
    assert!(matches!(
        h.manager
            .update_connection_tokens(&refresh_started_from, "late-access".to_string(), None, None)
            .await,
        Err(GitProviderManagerError::ConnectionGrantReplaced {
            connection_id: CONNECTION_ID
        })
    ));
    let stored = h.decrypt(h.row().await.access_token.as_deref());
    assert_ne!(stored, "late-access");
    assert_ne!(stored, token_after_first, "the later update must be kept");
}

#[tokio::test]
async fn a_connection_deleted_during_validation_is_named_in_the_error() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let gate = h.gate_user_route();

    let (result, ()) = tokio::join!(
        h.manager
            .update_connection_token(CONNECTION_ID, "orphan-access".to_string(), None),
        async {
            h.wait_for_user_calls(1).await;
            git_provider_connections::Entity::delete_by_id(CONNECTION_ID)
                .exec(h.db.as_ref())
                .await
                .unwrap();
            gate.add_permits(1);
        }
    );

    assert!(matches!(
        &result,
        Err(GitProviderManagerError::ConnectionNotFound(message))
            if message.contains(&CONNECTION_ID.to_string())
    ));
}
