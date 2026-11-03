// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use axum::http::Method;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement};
use std::sync::atomic::{AtomicUsize, Ordering};
use temps_auth::permissions::Role;

struct NoModel;

#[async_trait::async_trait]
impl temps_ai::AiService for NoModel {
    async fn is_available(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        _request: temps_ai::AiRequest,
    ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
        Err(temps_ai::AiError::NotAvailable)
    }

    async fn chat_stream(
        &self,
        _request: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
        Err(temps_ai::AiError::NotAvailable)
    }

    async fn chat_stream_turn(
        &self,
        _request: temps_ai::ChatTurnRequest,
    ) -> Result<temps_ai::ChatTurnStream, temps_ai::AiError> {
        Err(temps_ai::AiError::NotAvailable)
    }
}

struct NoQueue;

#[async_trait::async_trait]
impl temps_core::JobQueue for NoQueue {
    async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
        Ok(())
    }
    fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
        panic!("authorization tests never subscribe to the job queue")
    }
}

#[derive(Default)]
struct CountingAudit(AtomicUsize);

#[async_trait::async_trait]
impl AuditLogger for CountingAudit {
    async fn create_audit_log(
        &self,
        _operation: &dyn temps_core::AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Harness {
    state: Arc<AppState>,
    db: Arc<DatabaseConnection>,
    audit: Arc<CountingAudit>,
}

impl Harness {
    fn new(mock: MockDatabase) -> Self {
        Self::over(Arc::new(mock.into_connection()))
    }

    /// The same state over any database, real or mock.
    fn over(db: Arc<DatabaseConnection>) -> Self {
        let encryption = Arc::new(
            temps_core::EncryptionService::new(&"0".repeat(64)).expect("test encryption key"),
        );
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("test server config"),
        );
        let config = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let queue: Arc<dyn temps_core::JobQueue> = Arc::new(NoQueue);
        // Never dialled: these handlers do not reach Docker, and an HTTP
        // client needs no socket to exist, so the tests run without one.
        let docker = Arc::new(
            bollard::Docker::connect_with_http(
                "http://127.0.0.1:2375",
                4,
                bollard::API_DEFAULT_VERSION,
            )
            .expect("unconnected Docker client"),
        );
        let external = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption.clone(),
            docker,
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));
        let git = Arc::new(temps_git::GitProviderManager::new(
            db.clone(),
            encryption.clone(),
            queue.clone(),
            config.clone(),
        ));
        let environments = Arc::new(temps_environments::EnvironmentService::new(
            db.clone(),
            config.clone(),
        ));
        let project_service = Arc::new(temps_projects::ProjectService::new(
            db.clone(),
            queue,
            config,
            external,
            git,
            environments,
            encryption.clone(),
        ));
        let audit = Arc::new(CountingAudit::default());
        let state = Arc::new(AppState {
            service: Arc::new(crate::ConversationService::new(
                db.clone(),
                Arc::new(NoModel),
                Vec::new(),
            )),
            db: db.clone(),
            audit_service: audit.clone(),
            pending_actions: Arc::new(PendingActionService::new(
                db.clone(),
                Arc::new(temps_ai_api_tools::WriteApiToolsHandle::new()),
                encryption.clone(),
            )),
            applications: Arc::new(crate::ApplicationService::new(db.clone())),
            goals: Arc::new(crate::goals::GoalService::new(db.clone())),
            secret_slots: Arc::new(crate::secret_slots::SecretSlotService::new(
                db.clone(),
                encryption,
            )),
            assistant_attachments: Arc::new(crate::attachments::AssistantAttachmentService::new(
                db.clone(),
            )),
            attachment_uploads: Arc::new(crate::attachment_uploads::UploadSessions::new()),
            project_service,
            application_workspaces: Arc::new(crate::ApplicationWorkspaceService::new(
                std::env::temp_dir(),
            )),
            config_service: None,
            application_sandboxes: None,
            sandbox_snapshots: None,
            source_drop_deployer: None,
            project_access_checker: None,
            application_terminal: None,
        });
        Self { state, db, audit }
    }

    fn state(&self) -> State<Arc<AppState>> {
        State(self.state.clone())
    }

    /// Release the state and return the statements the handler issued and
    /// the number of audit entries it wrote.
    fn finish(self) -> (Vec<Statement>, usize) {
        let Harness { state, db, audit } = self;
        drop(state);
        let db = Arc::try_unwrap(db)
            .unwrap_or_else(|_| panic!("a service outlived the state it was built for"));
        let statements = db
            .into_transaction_log()
            .into_iter()
            .flat_map(|transaction| transaction.statements().to_vec())
            .collect();
        (statements, audit.0.load(Ordering::SeqCst))
    }
}

fn writes(statements: &[Statement]) -> Vec<&Statement> {
    statements
        .iter()
        .filter(|statement| !statement.sql.trim_start().starts_with("SELECT"))
        .collect()
}

/// Whether `statement` binds `value`, e.g. the status it writes.
fn binds(statement: &Statement, value: &str) -> bool {
    format!("{:?}", statement.values).contains(value)
}

fn owner() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 1,
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

/// An API key of `role` owned by the conversation's owner.
fn key(role: Role) -> RequireAuth {
    RequireAuth(auth_of(1, Some(role), None))
}

/// An API key owned by user `user_id`, with a role, explicit permissions,
/// or both, as API keys are issued.
fn auth_of(user_id: i32, role: Option<Role>, permissions: Option<Vec<Permission>>) -> AuthContext {
    let mut user = owner();
    user.id = user_id;
    AuthContext::new_api_key(user, role, permissions, "test-key".to_string(), 1)
}

fn metadata() -> Extension<RequestMetadata> {
    Extension(RequestMetadata {
        ip_address: "203.0.113.7".to_string(),
        user_agent: "authorization-test".to_string(),
        headers: HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost:3000".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    })
}

fn conversation(context_type: &str, status: &str, turn_status: &str) -> ai_conversations::Model {
    let now = chrono::Utc::now();
    let assistant = context_type == crate::providers::assistant::ASSISTANT_CONTEXT_TYPE;
    ai_conversations::Model {
        id: 10,
        public_id: "conv_1".to_string(),
        project_id: (!assistant).then_some(1),
        application_id: None,
        context_type: context_type.to_string(),
        context_id: "1".to_string(),
        title: None,
        status: status.to_string(),
        kind: "ai".to_string(),
        created_by: 1,
        metadata: None,
        created_at: now,
        last_activity_at: now,
        ai_provider: "openrouter".to_string(),
        ai_model: "default".to_string(),
        ai_thinking_level: None,
        ai_permission_mode: "ask".to_string(),
        cli_session_id: None,
        cli_session_fingerprint: None,
        turn_status: turn_status.to_string(),
        active_turn_id: None,
        last_turn_id: None,
        turn_started_at: None,
        next_step_suggestions: None,
    }
}

fn with_status(mut conversation: ai_conversations::Model, status: &str) -> ai_conversations::Model {
    conversation.status = status.to_string();
    conversation
}

fn proposed_action(required_permission: Option<&str>) -> ai_pending_actions::Model {
    let now = chrono::Utc::now();
    ai_pending_actions::Model {
        id: 20,
        public_id: "act_1".to_string(),
        conversation_id: 10,
        message_id: None,
        project_id: Some(1),
        plan_public_id: None,
        step_index: 0,
        operation_id: "create_deployment".to_string(),
        method: "POST".to_string(),
        summary: "POST /projects/1/deployments".to_string(),
        params: serde_json::json!({}),
        required_permission: required_permission.map(str::to_string),
        status: "proposed".to_string(),
        result: None,
        error: None,
        created_by: 1,
        confirmed_by: None,
        created_at: now,
        confirmed_at: None,
        executed_at: None,
    }
}

fn status_of<T: IntoResponse>(result: Result<T, Problem>) -> StatusCode {
    match result {
        Ok(response) => response.into_response().status(),
        Err(problem) => problem.status_code,
    }
}

/// One mutation route, called with a key and a conversation lookup.
#[derive(Clone, Copy, Debug)]
enum Mutation {
    Rename,
    Archive,
    Restore,
    Stop,
}

impl Mutation {
    const ALL: [Mutation; 4] = [
        Mutation::Rename,
        Mutation::Archive,
        Mutation::Restore,
        Mutation::Stop,
    ];

    async fn call(self, harness: &Harness, auth: RequireAuth) -> StatusCode {
        self.call_on(harness, auth, "conv_1").await
    }

    async fn call_on(self, harness: &Harness, auth: RequireAuth, id: &str) -> StatusCode {
        let id = Path(id.to_string());
        match self {
            Mutation::Rename => status_of(
                rename_user_conversation(
                    auth,
                    harness.state(),
                    metadata(),
                    id,
                    Json(RenameConversationRequest {
                        title: "Renamed by the key".to_string(),
                    }),
                )
                .await,
            ),
            Mutation::Archive => {
                status_of(archive_user_conversation(auth, harness.state(), metadata(), id).await)
            }
            Mutation::Restore => {
                status_of(restore_user_conversation(auth, harness.state(), metadata(), id).await)
            }
            Mutation::Stop => status_of(stop_user_turn(auth, harness.state(), id).await),
        }
    }
}

// The issue: a reader key owned by the conversation's owner changed it.
// Every route now refuses it after the ownership lookup, before writing
// or auditing anything.
#[tokio::test]
async fn read_only_keys_cannot_mutate_their_owners_conversations() {
    for mutation in Mutation::ALL {
        for context_type in ["project", "deployment", "global"] {
            for role in [Role::Reader, Role::ApiReader] {
                let harness = Harness::new(
                    MockDatabase::new(DatabaseBackend::Postgres).append_query_results(vec![vec![
                        conversation(context_type, "active", "running"),
                    ]]),
                );

                let status = mutation.call(&harness, key(role.clone())).await;

                let (statements, audits) = harness.finish();
                let case = format!("{mutation:?} by {role:?} on a {context_type} chat");
                assert_eq!(status, StatusCode::FORBIDDEN, "{case}");
                assert_eq!(statements.len(), 1, "{case}: only the ownership lookup");
                assert!(writes(&statements).is_empty(), "{case}: {statements:?}");
                assert_eq!(audits, 0, "{case}");
            }
        }
    }
}

// The same routes still work for a key that holds projects:write, and
// each makes exactly the state change it names.
#[tokio::test]
async fn project_writers_still_rename_archive_restore_and_stop() {
    let cases = [
        (
            Mutation::Rename,
            conversation("project", "active", "idle"),
            StatusCode::OK,
            "Renamed by the key",
        ),
        (
            Mutation::Archive,
            conversation("project", "active", "idle"),
            StatusCode::NO_CONTENT,
            "archived",
        ),
        (
            Mutation::Restore,
            conversation("project", "archived", "idle"),
            StatusCode::NO_CONTENT,
            "active",
        ),
    ];
    for (mutation, before, expected, change) in cases {
        let updated = with_status(before.clone(), "updated");
        let harness = Harness::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .append_query_results(vec![vec![updated]]),
        );

        let status = mutation.call(&harness, key(Role::User)).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, expected, "{mutation:?}");
        let writes = writes(&statements);
        assert_eq!(writes.len(), 1, "{mutation:?}: {statements:?}");
        assert!(
            binds(writes[0], change),
            "{mutation:?} must write {change}: {:?}",
            writes[0]
        );
        assert_eq!(audits, 1, "{mutation:?}");
    }

    // Stopping clears a running turn with a guarded update.
    let harness = Harness::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![conversation("project", "active", "running")]])
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]),
    );
    let status = Mutation::Stop.call(&harness, key(Role::User)).await;
    let (statements, _) = harness.finish();
    assert_eq!(status, StatusCode::NO_CONTENT);
    let writes = writes(&statements);
    assert_eq!(writes.len(), 1, "{statements:?}");
    assert!(binds(writes[0], "cancelled"), "{:?}", writes[0]);
}

// Permission is decided before the archive's own state rule: a reader
// learns nothing about a running turn, a writer gets the real conflict,
// and neither changes the conversation.
#[tokio::test]
async fn archiving_a_running_turn_is_refused_without_a_write() {
    for (role, expected) in [
        (Role::Reader, StatusCode::FORBIDDEN),
        (Role::User, StatusCode::CONFLICT),
    ] {
        let harness = Harness::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![conversation("project", "active", "running")]]),
        );

        let status = Mutation::Archive.call(&harness, key(role.clone())).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, expected, "{role:?}");
        assert!(writes(&statements).is_empty(), "{role:?}: {statements:?}");
        assert_eq!(audits, 0, "{role:?}");
    }
}

// A reader may start an assistant chat and message it, so it may also
// archive and restore it; the archive round trip still lands.
#[tokio::test]
async fn readers_still_manage_their_assistant_chats() {
    for (mutation, before, change) in [
        (
            Mutation::Archive,
            conversation("assistant", "active", "idle"),
            "archived",
        ),
        (
            Mutation::Restore,
            conversation("assistant", "archived", "idle"),
            "active",
        ),
    ] {
        let updated = with_status(before.clone(), "updated");
        let harness = Harness::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![before]])
                .append_query_results(vec![vec![updated]]),
        );

        let status = mutation.call(&harness, key(Role::Reader)).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::NO_CONTENT, "{mutation:?}");
        let writes = writes(&statements);
        assert_eq!(writes.len(), 1, "{mutation:?}: {statements:?}");
        assert!(binds(writes[0], change), "{:?}", writes[0]);
        assert_eq!(audits, 1, "{mutation:?}");
    }
}

async fn reject(harness: &Harness, auth: RequireAuth) -> StatusCode {
    reject_on(harness, auth, "act_1").await
}

async fn reject_on(harness: &Harness, auth: RequireAuth, id: &str) -> StatusCode {
    status_of(
        reject_user_pending_action(
            auth,
            None,
            harness.state(),
            metadata(),
            Path(id.to_string()),
        )
        .await,
    )
}

/// The reads every reject makes before deciding: the owned action, its
/// conversation, and the service's own load of the action.
fn reject_lookups(action: ai_pending_actions::Model) -> MockDatabase {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![action.clone()]])
        .append_query_results(vec![vec![conversation("assistant", "active", "idle")]])
        .append_query_results(vec![vec![action]])
}

// The issue: a reader key rejected a proposal its owner meant to approve.
// It is refused before the claim, so the proposal stays proposed.
#[tokio::test]
async fn read_only_keys_cannot_reject_their_owners_proposals() {
    for permission in [Some("deployments:create"), None] {
        for role in [Role::Reader, Role::ApiReader] {
            let harness = Harness::new(reject_lookups(proposed_action(permission)));

            let status = reject(&harness, key(role.clone())).await;

            let (statements, audits) = harness.finish();
            let case = format!("{role:?} rejecting an action requiring {permission:?}");
            assert_eq!(status, StatusCode::FORBIDDEN, "{case}");
            assert_eq!(statements.len(), 3, "{case}: lookups only");
            assert!(writes(&statements).is_empty(), "{case}: {statements:?}");
            assert_eq!(audits, 0, "{case}");
        }
    }
}

// A key that could approve the action may reject it. The claim is still
// the atomic, status-guarded update, so a concurrent confirm that got
// there first wins and this reject reports a conflict instead.
#[tokio::test]
async fn rejecting_still_claims_the_proposal_atomically() {
    let mut rejected = proposed_action(Some("deployments:create"));
    rejected.status = "rejected".to_string();
    let harness = Harness::new(
        reject_lookups(proposed_action(Some("deployments:create")))
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![rejected]]),
    );

    let status = reject(&harness, key(Role::User)).await;

    let (statements, audits) = harness.finish();
    assert_eq!(status, StatusCode::OK);
    let writes = writes(&statements);
    assert_eq!(writes.len(), 1, "{statements:?}");
    let claim = writes[0];
    assert!(binds(claim, "rejected"), "{claim:?}");
    assert!(
        claim.sql.contains(r#"WHERE"#) && binds(claim, "proposed"),
        "the claim must only flip a still-proposed row: {claim:?}"
    );
    assert_eq!(audits, 1);

    let harness = Harness::new(
        reject_lookups(proposed_action(Some("deployments:create"))).append_exec_results(vec![
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            },
        ]),
    );

    let status = reject(&harness, key(Role::User)).await;

    let (_, audits) = harness.finish();
    assert_eq!(status, StatusCode::CONFLICT, "the race loser is told so");
    assert_eq!(audits, 0);
}

// --- Through the assembled router -------------------------------------

/// The five routes from the issue, as the router serves them.
fn issue_routes() -> [(Method, &'static str, &'static str); 5] {
    [
        (
            Method::PATCH,
            "/ai/conversations/conv_1",
            r#"{"title":"Renamed by the key"}"#,
        ),
        (Method::POST, "/ai/conversations/conv_1/archive", ""),
        (Method::POST, "/ai/conversations/conv_1/restore", ""),
        (Method::POST, "/ai/conversations/conv_1/stop", ""),
        (Method::POST, "/ai/pending-actions/act_1/reject", ""),
    ]
}

fn is_reject(uri: &str) -> bool {
    uri.starts_with("/ai/pending-actions/")
}

/// Send one request through `configure_routes`, with the extensions the
/// console's middleware would have attached.
async fn send(
    harness: &Harness,
    method: Method,
    uri: &str,
    body: &str,
    auth: Option<AuthContext>,
) -> StatusCode {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("valid test request");
    if let Some(auth) = auth {
        request.extensions_mut().insert(auth);
    }
    let Extension(metadata) = metadata();
    request.extensions_mut().insert(metadata);
    configure_routes()
        .with_state(harness.state.clone())
        .oneshot(request)
        .await
        .expect("the router is infallible")
        .status()
}

// Without credentials every route answers 401 before touching anything.
#[tokio::test]
async fn the_router_requires_authentication_on_every_issue_route() {
    for (method, uri, body) in issue_routes() {
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));

        let status = send(&harness, method.clone(), uri, body, None).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert!(statements.is_empty(), "{method} {uri}: {statements:?}");
        assert_eq!(audits, 0, "{method} {uri}");
    }
}

// The issue's exact requests: a reader key, through the router, on its
// owner's conversation and proposal.
#[tokio::test]
async fn the_router_refuses_read_only_keys_on_every_issue_route() {
    for (method, uri, body) in issue_routes() {
        for role in [Role::Reader, Role::ApiReader] {
            let mock = if is_reject(uri) {
                reject_lookups(proposed_action(Some("deployments:create")))
            } else {
                MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_results(vec![vec![conversation("project", "active", "running")]])
            };
            let harness = Harness::new(mock);

            let status = send(
                &harness,
                method.clone(),
                uri,
                body,
                Some(auth_of(1, Some(role.clone()), None)),
            )
            .await;

            let (statements, audits) = harness.finish();
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri} as {role:?}");
            assert!(
                writes(&statements).is_empty(),
                "{method} {uri}: {statements:?}"
            );
            assert_eq!(audits, 0, "{method} {uri}");
        }
    }
}

// Deployment tokens are machine credentials for one project, never a
// principal that owns conversations: refused before any lookup.
#[tokio::test]
async fn deployment_tokens_are_refused_before_any_lookup() {
    for (method, uri, body) in issue_routes() {
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));
        let token = AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "deploy".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
        );

        let status = send(&harness, method.clone(), uri, body, Some(token)).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert!(statements.is_empty(), "{method} {uri}: {statements:?}");
        assert_eq!(audits, 0, "{method} {uri}");
    }
}

// A key owned by someone else, even an administrator's, cannot see the
// conversation or proposal at all: the lookup is scoped to the caller,
// so it answers 404 and never reaches the permission check or a write.
#[tokio::test]
async fn another_users_key_cannot_find_the_conversation_or_proposal() {
    for (method, uri, body) in issue_routes() {
        let mock = MockDatabase::new(DatabaseBackend::Postgres);
        let mock = if is_reject(uri) {
            mock.append_query_results(vec![Vec::<ai_pending_actions::Model>::new()])
        } else {
            mock.append_query_results(vec![Vec::<ai_conversations::Model>::new()])
        };
        let harness = Harness::new(mock);

        let status = send(
            &harness,
            method.clone(),
            uri,
            body,
            Some(auth_of(2, Some(Role::Admin), None)),
        )
        .await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(statements.len(), 1, "{method} {uri}: {statements:?}");
        assert!(
            statements[0].sql.contains(r#""created_by" = "#)
                && binds(&statements[0], "Int(Some(2))"),
            "{method} {uri}: the lookup must be scoped to the caller: {:?}",
            statements[0]
        );
        assert_eq!(audits, 0, "{method} {uri}");
    }
}

// --- Permission shapes beyond the built-in read-only roles ------------

// A key issued with explicit permissions instead of a role is judged by
// those permissions: projects:read alone is refused, adding
// projects:write is enough.
#[tokio::test]
async fn custom_permission_keys_are_judged_by_their_permissions() {
    for mutation in Mutation::ALL {
        let harness = Harness::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![conversation("project", "active", "running")]]),
        );
        let read_only = RequireAuth(auth_of(1, None, Some(vec![Permission::ProjectsRead])));

        let status = mutation.call(&harness, read_only).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::FORBIDDEN, "{mutation:?}");
        assert!(
            writes(&statements).is_empty(),
            "{mutation:?}: {statements:?}"
        );
        assert_eq!(audits, 0, "{mutation:?}");
    }

    let before = conversation("project", "active", "idle");
    let harness = Harness::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![before.clone()]])
            .append_query_results(vec![vec![with_status(before, "archived")]]),
    );
    let writer = RequireAuth(auth_of(
        1,
        None,
        Some(vec![Permission::ProjectsRead, Permission::ProjectsWrite]),
    ));

    let status = Mutation::Archive.call(&harness, writer).await;

    let (statements, audits) = harness.finish();
    assert_eq!(status, StatusCode::NO_CONTENT);
    let writes = writes(&statements);
    assert_eq!(writes.len(), 1, "{statements:?}");
    assert!(binds(writes[0], "archived"), "{:?}", writes[0]);
    assert_eq!(audits, 1);
}

// The project-scoped versions the issue compared against keep refusing
// read-only keys, before even loading the conversation.
#[tokio::test]
async fn project_scoped_routes_refuse_read_only_keys_before_any_lookup() {
    for role in [Role::Reader, Role::ApiReader] {
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));
        let conversation_path = || Path((1, "conv_1".to_string()));

        let statuses = [
            status_of(stop_turn(key(role.clone()), harness.state(), conversation_path()).await),
            status_of(
                archive_conversation(
                    key(role.clone()),
                    harness.state(),
                    metadata(),
                    conversation_path(),
                )
                .await,
            ),
            status_of(
                rename_conversation(
                    key(role.clone()),
                    harness.state(),
                    metadata(),
                    conversation_path(),
                    Json(RenameConversationRequest {
                        title: "Renamed by the key".to_string(),
                    }),
                )
                .await,
            ),
            status_of(
                reject_pending_action(
                    key(role.clone()),
                    None,
                    harness.state(),
                    metadata(),
                    Path((1, "act_1".to_string())),
                )
                .await,
            ),
        ];

        let (statements, audits) = harness.finish();
        assert!(
            statuses
                .iter()
                .all(|status| *status == StatusCode::FORBIDDEN),
            "{role:?}: {statuses:?}"
        );
        assert!(statements.is_empty(), "{role:?}: {statements:?}");
        assert_eq!(audits, 0, "{role:?}");
    }
}

// A platform administrator holds users:create but not projects:write.
// It may reject a proposal it could approve, and not one that declares no
// permission, which falls back to projects:write.
#[tokio::test]
async fn platform_admins_reject_what_they_could_approve() {
    let mut rejected = proposed_action(Some("users:create"));
    rejected.status = "rejected".to_string();
    let harness = Harness::new(
        reject_lookups(proposed_action(Some("users:create")))
            .append_exec_results(vec![MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results(vec![vec![rejected]]),
    );

    let status = reject(&harness, key(Role::PlatformAdmin)).await;

    let (statements, audits) = harness.finish();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(writes(&statements).len(), 1, "{statements:?}");
    assert_eq!(audits, 1);

    let harness = Harness::new(reject_lookups(proposed_action(None)));

    let status = reject(&harness, key(Role::PlatformAdmin)).await;

    let (statements, audits) = harness.finish();
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(writes(&statements).is_empty(), "{statements:?}");
    assert_eq!(audits, 0);
}

// --- Against a migrated PostgreSQL ------------------------------------
//
// These boot a TimescaleDB container and skip (not fail) when no
// container runtime is available.

use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};
use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

async fn database(test: &str) -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(db) => Some(db),
        Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
            println!("Skipping {test}: {error}");
            None
        }
        Err(error) => panic!("{test}: database setup failed: {error}"),
    }
}

async fn insert_user(db: &DatabaseConnection, email: &str) -> i32 {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
             created_at, updated_at) VALUES ('Owner', $1, true, false, false, NOW(), NOW()) \
             RETURNING id",
        [email.into()],
    ))
    .await
    .expect("insert user")
    .expect("a row")
    .try_get::<i32>("", "id")
    .expect("id")
}

async fn insert_chat(
    db: &DatabaseConnection,
    user_id: i32,
    public_id: &str,
    turn_status: &str,
) -> ai_conversations::Model {
    let now = chrono::Utc::now();
    ai_conversations::ActiveModel {
        public_id: Set(public_id.to_string()),
        project_id: Set(None),
        application_id: Set(None),
        context_type: Set("global".to_string()),
        context_id: Set(format!("global-{public_id}")),
        title: Set(Some("Original title".to_string())),
        status: Set("active".to_string()),
        kind: Set("ai".to_string()),
        created_by: Set(user_id),
        metadata: Set(None),
        created_at: Set(now),
        last_activity_at: Set(now),
        ai_provider: Set("codex".to_string()),
        ai_model: Set("default".to_string()),
        ai_thinking_level: Set(None),
        ai_permission_mode: Set("ask".to_string()),
        cli_session_id: Set(None),
        cli_session_fingerprint: Set(None),
        turn_status: Set(turn_status.to_string()),
        active_turn_id: Set(None),
        last_turn_id: Set(None),
        turn_started_at: Set(None),
        next_step_suggestions: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert chat")
}

async fn insert_action(
    db: &DatabaseConnection,
    user_id: i32,
    conversation_id: i64,
    public_id: &str,
) -> ai_pending_actions::Model {
    ai_pending_actions::ActiveModel {
        public_id: Set(public_id.to_string()),
        conversation_id: Set(conversation_id),
        message_id: Set(None),
        project_id: Set(None),
        plan_public_id: Set(None),
        step_index: Set(0),
        operation_id: Set("create_deployment".to_string()),
        method: Set("POST".to_string()),
        summary: Set("POST /projects/1/deployments".to_string()),
        params: Set(serde_json::json!({})),
        required_permission: Set(Some("deployments:create".to_string())),
        status: Set("proposed".to_string()),
        result: Set(None),
        error: Set(None),
        created_by: Set(user_id),
        confirmed_by: Set(None),
        created_at: Set(chrono::Utc::now()),
        confirmed_at: Set(None),
        executed_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert pending action")
}

async fn reload_chat(db: &DatabaseConnection, id: i64) -> ai_conversations::Model {
    ai_conversations::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read chat")
        .expect("chat exists")
}

async fn reload_action(db: &DatabaseConnection, id: i64) -> ai_pending_actions::Model {
    ai_pending_actions::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read pending action")
        .expect("pending action exists")
}

// End to end on real tables: every refused call leaves the rows exactly
// as they were, and the same calls from a writer make each change.
#[tokio::test]
async fn refused_calls_leave_real_rows_untouched() {
    let Some(test_db) = database("refused_calls_leave_real_rows_untouched").await else {
        return;
    };
    let db = test_db.db.clone();
    let owner_id = insert_user(&db, "owner@example.test").await;
    let chat = insert_chat(&db, owner_id, "conv_real", "running").await;
    let action = insert_action(&db, owner_id, chat.id, "act_real").await;
    let harness = Harness::over(db.clone());

    for role in [Role::Reader, Role::ApiReader] {
        let reader = || RequireAuth(auth_of(owner_id, Some(role.clone()), None));
        for mutation in Mutation::ALL {
            let status = mutation.call_on(&harness, reader(), "conv_real").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{mutation:?} as {role:?}");
        }
        let status = reject_on(&harness, reader(), "act_real").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "reject as {role:?}");
    }

    let unchanged = reload_chat(&db, chat.id).await;
    assert_eq!(unchanged.title.as_deref(), Some("Original title"));
    assert_eq!(unchanged.status, "active");
    assert_eq!(unchanged.turn_status, "running");
    let unchanged = reload_action(&db, action.id).await;
    assert_eq!(unchanged.status, "proposed");
    assert_eq!(unchanged.confirmed_by, None);
    assert_eq!(harness.audit.0.load(Ordering::SeqCst), 0);

    let writer = || RequireAuth(auth_of(owner_id, Some(Role::User), None));
    // Stop first: a running turn cannot be archived.
    assert_eq!(
        Mutation::Stop
            .call_on(&harness, writer(), "conv_real")
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(reload_chat(&db, chat.id).await.turn_status, "cancelled");
    assert_eq!(
        Mutation::Rename
            .call_on(&harness, writer(), "conv_real")
            .await,
        StatusCode::OK
    );
    assert_eq!(
        reload_chat(&db, chat.id).await.title.as_deref(),
        Some("Renamed by the key")
    );
    assert_eq!(
        Mutation::Archive
            .call_on(&harness, writer(), "conv_real")
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(reload_chat(&db, chat.id).await.status, "archived");
    assert_eq!(
        Mutation::Restore
            .call_on(&harness, writer(), "conv_real")
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(reload_chat(&db, chat.id).await.status, "active");
    assert_eq!(
        reject_on(&harness, writer(), "act_real").await,
        StatusCode::OK
    );
    let rejected = reload_action(&db, action.id).await;
    assert_eq!(rejected.status, "rejected");
    assert_eq!(rejected.confirmed_by, Some(owner_id));
    // Rename, archive, restore and reject are audited; stop is not.
    assert_eq!(harness.audit.0.load(Ordering::SeqCst), 4);
}

// Archive is a soft, idempotent state change on a real row: repeating it
// or restoring an already active chat succeeds without touching anything
// else, the row is never deleted, and an archived chat is still guarded,
// so a read-only key cannot bring it back.
#[tokio::test]
async fn archive_and_restore_are_idempotent_soft_state_changes() {
    let Some(test_db) = database("archive_and_restore_are_idempotent_soft_state_changes").await
    else {
        return;
    };
    let db = test_db.db.clone();
    let owner_id = insert_user(&db, "archiver@example.test").await;
    let chat = insert_chat(&db, owner_id, "conv_soft", "idle").await;
    let harness = Harness::over(db.clone());
    let writer = || RequireAuth(auth_of(owner_id, Some(Role::User), None));
    let reader = || RequireAuth(auth_of(owner_id, Some(Role::Reader), None));
    let assert_untouched = |row: &ai_conversations::Model, status: &str| {
        assert_eq!(row.status, status);
        assert_eq!(row.title.as_deref(), Some("Original title"));
        assert_eq!(row.turn_status, "idle");
        assert_eq!(row.created_by, owner_id);
    };

    // Restoring a chat that was never archived is a no-op success.
    assert_eq!(
        Mutation::Restore
            .call_on(&harness, writer(), "conv_soft")
            .await,
        StatusCode::NO_CONTENT
    );
    assert_untouched(&reload_chat(&db, chat.id).await, "active");

    for _ in 0..2 {
        assert_eq!(
            Mutation::Archive
                .call_on(&harness, writer(), "conv_soft")
                .await,
            StatusCode::NO_CONTENT
        );
        assert_untouched(&reload_chat(&db, chat.id).await, "archived");
    }

    assert_eq!(
        Mutation::Restore
            .call_on(&harness, reader(), "conv_soft")
            .await,
        StatusCode::FORBIDDEN
    );
    assert_untouched(&reload_chat(&db, chat.id).await, "archived");

    for _ in 0..2 {
        assert_eq!(
            Mutation::Restore
                .call_on(&harness, writer(), "conv_soft")
                .await,
            StatusCode::NO_CONTENT
        );
        assert_untouched(&reload_chat(&db, chat.id).await, "active");
    }
    // Five successful writes, each audited; the refused restore is not.
    assert_eq!(harness.audit.0.load(Ordering::SeqCst), 5);
}

fn race_service(db: &Arc<DatabaseConnection>) -> Arc<PendingActionService> {
    Arc::new(PendingActionService::new(
        db.clone(),
        Arc::new(temps_ai_api_tools::WriteApiToolsHandle::new()),
        Arc::new(temps_core::EncryptionService::new(&"0".repeat(64)).expect("test encryption key")),
    ))
}

// Several authorised rejects racing on one proposal: the status-guarded
// claim lets exactly one through, and every other is told it lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_rejects_claim_a_proposal_exactly_once() {
    let Some(test_db) = database("concurrent_rejects_claim_a_proposal_exactly_once").await else {
        return;
    };
    let db = test_db.db.clone();
    let owner_id = insert_user(&db, "racer@example.test").await;
    let chat = insert_chat(&db, owner_id, "conv_race", "idle").await;
    let service = race_service(&db);

    for round in 0..20 {
        let public_id = format!("act_race_{round}");
        let action = insert_action(&db, owner_id, chat.id, &public_id).await;
        let attempts: Vec<_> = (0..4)
            .map(|_| {
                let service = service.clone();
                let public_id = public_id.clone();
                let auth = auth_of(owner_id, Some(Role::User), None);
                tokio::spawn(async move {
                    service
                        .reject(None, &public_id, &auth, Some(owner_id))
                        .await
                })
            })
            .collect();

        let (mut won, mut lost) = (0, 0);
        for attempt in attempts {
            match attempt.await.expect("reject task") {
                Ok(row) => {
                    assert_eq!(row.status, "rejected", "round {round}");
                    won += 1;
                }
                Err(PendingActionError::InvalidState { .. }) => lost += 1,
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!((won, lost), (1, 3), "round {round}");
        assert_eq!(reload_action(&db, action.id).await.status, "rejected");
    }
}

// A read-only key racing an authorised reject never claims the proposal:
// it is refused (or, arriving late, told the proposal is already
// decided), and the authorised reject always wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_reject_racing_an_allowed_one_never_claims() {
    let Some(test_db) = database("a_refused_reject_racing_an_allowed_one_never_claims").await
    else {
        return;
    };
    let db = test_db.db.clone();
    let owner_id = insert_user(&db, "mixed-racer@example.test").await;
    let chat = insert_chat(&db, owner_id, "conv_mixed_race", "idle").await;
    let service = race_service(&db);

    for round in 0..20 {
        let public_id = format!("act_mixed_{round}");
        let action = insert_action(&db, owner_id, chat.id, &public_id).await;
        let spawn_reject = |role: Role| {
            let service = service.clone();
            let public_id = public_id.clone();
            let auth = auth_of(owner_id, Some(role), None);
            tokio::spawn(async move {
                service
                    .reject(None, &public_id, &auth, Some(owner_id))
                    .await
            })
        };
        let reader = spawn_reject(Role::Reader);
        let writer = spawn_reject(Role::User);

        match reader.await.expect("reader task") {
            Err(PendingActionError::PermissionDenied { .. })
            | Err(PendingActionError::InvalidState { .. }) => {}
            other => panic!("round {round}: a read-only key must never reject: {other:?}"),
        }
        let won = writer
            .await
            .expect("writer task")
            .unwrap_or_else(|error| panic!("round {round}: the writer must win: {error:?}"));
        assert_eq!(won.status, "rejected", "round {round}");
        let stored = reload_action(&db, action.id).await;
        assert_eq!(stored.status, "rejected", "round {round}");
        assert_eq!(stored.confirmed_by, Some(owner_id), "round {round}");
    }
}

/// Every route `configure_routes` serves, tagged with the handler module it
/// lives in, so each group of handlers moved out of the old `handlers.rs`
/// keeps an authentication and authorization test through the router. The
/// last field says whether the handler's own guards answer a credentialed
/// request: WebSocket routes reject a plain request in their upgrade
/// extractor, and the attachment upload in its multipart extractor, before
/// the handler runs, so those are covered for 401 here and by focused guard
/// tests elsewhere.
fn handler_group_routes() -> Vec<(&'static str, Method, &'static str, &'static str, bool)> {
    vec![
        // applications
        ("applications", Method::GET, "/ai/applications", "", true),
        (
            "applications",
            Method::POST,
            "/ai/applications",
            r#"{"name":"Shop","description":null}"#,
            true,
        ),
        (
            "applications",
            Method::GET,
            "/ai/applications/app_1",
            "",
            true,
        ),
        (
            "applications",
            Method::PATCH,
            "/ai/applications/app_1",
            r#"{"name":"Store"}"#,
            true,
        ),
        (
            "applications",
            Method::DELETE,
            "/ai/applications/app_1",
            "",
            true,
        ),
        (
            "applications",
            Method::POST,
            "/ai/applications/app_1/restore",
            "",
            true,
        ),
        (
            "applications",
            Method::GET,
            "/ai/applications/app_1/conversations",
            "",
            true,
        ),
        (
            "applications",
            Method::POST,
            "/ai/applications/app_1/conversations",
            r#"{"ai_provider":null}"#,
            true,
        ),
        // application_projects
        (
            "application_projects",
            Method::POST,
            "/ai/applications/app_1/projects",
            r#"{"name":"web"}"#,
            true,
        ),
        (
            "application_projects",
            Method::POST,
            "/ai/applications/app_1/projects/link",
            r#"{"project_id":1}"#,
            true,
        ),
        (
            "application_projects",
            Method::DELETE,
            "/ai/applications/app_1/projects/1",
            "",
            true,
        ),
        (
            "application_projects",
            Method::POST,
            "/ai/applications/app_1/projects/1/primary",
            "",
            true,
        ),
        (
            "application_projects",
            Method::POST,
            "/ai/applications/app_1/projects/1/deploy",
            "{}",
            true,
        ),
        // application_workspace
        (
            "application_workspace",
            Method::GET,
            "/ai/applications/app_1/workspace",
            "",
            true,
        ),
        (
            "application_workspace",
            Method::PATCH,
            "/ai/applications/app_1/workspace",
            r#"{"runtime":null,"image":null,"cpu_limit":null,"memory_limit_mb":null,"pids_limit":null,"disk_limit_mb":null,"idle_timeout_secs":null}"#,
            true,
        ),
        (
            "application_workspace",
            Method::POST,
            "/ai/applications/app_1/workspace/actions",
            r#"{"action":"stop","confirm":null,"runtime":null,"snapshot_id":null,"label":null}"#,
            true,
        ),
        // workspace_files
        (
            "workspace_files",
            Method::GET,
            "/ai/workspace/file-limits",
            "",
            true,
        ),
        (
            "workspace_files",
            Method::POST,
            "/ai/workspace/files",
            r#"{"files":[]}"#,
            true,
        ),
        (
            "workspace_files",
            Method::GET,
            "/ai/workspace/file/download?path=README.md",
            "",
            true,
        ),
        (
            "workspace_files",
            Method::POST,
            "/ai/applications/app_1/projects/1/workspace/source",
            r#"{"url":"https://example.com/repo.git","revision":null,"depth":null,"git_connection_id":null}"#,
            true,
        ),
        (
            "workspace_files",
            Method::POST,
            "/ai/applications/app_1/projects/1/workspace/files",
            r#"{"files":[]}"#,
            true,
        ),
        (
            "workspace_files",
            Method::POST,
            "/ai/applications/app_1/workspace/files",
            r#"{"files":[]}"#,
            true,
        ),
        (
            "workspace_files",
            Method::GET,
            "/ai/applications/app_1/workspace/file/download?path=README.md",
            "",
            true,
        ),
        // workspace_browse
        (
            "workspace_browse",
            Method::GET,
            "/ai/workspace/changes",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/workspace/directory",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/workspace/file?path=README.md",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/workspace/diff?path=README.md",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/applications/app_1/workspace/changes",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/applications/app_1/workspace/directory",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/applications/app_1/workspace/file?path=README.md",
            "",
            true,
        ),
        (
            "workspace_browse",
            Method::GET,
            "/ai/applications/app_1/workspace/diff?path=README.md",
            "",
            true,
        ),
        // workspace_terminal
        (
            "workspace_terminal",
            Method::GET,
            "/ai/workspace/terminal?thread=conv_1",
            "",
            false,
        ),
        (
            "workspace_terminal",
            Method::GET,
            "/ai/applications/app_1/workspace/terminal?thread=conv_1",
            "",
            false,
        ),
        // conversations
        (
            "conversations",
            Method::GET,
            "/ai/workspace-activity",
            "",
            true,
        ),
        ("conversations", Method::GET, "/ai/workspace", "", true),
        ("conversations", Method::GET, "/ai/conversations", "", true),
        (
            "conversations",
            Method::POST,
            "/ai/conversations",
            "{}",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/ai/assistant/conversations",
            "{}",
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/ai/conversations/conv_1",
            "",
            true,
        ),
        (
            "conversations",
            Method::PATCH,
            "/ai/conversations/conv_1",
            r#"{"title":"Renamed"}"#,
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/ai/conversations/conv_1/diagnostics",
            "",
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/ai/conversations/conv_1/usage",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/ai/conversations/conv_1/stop",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/ai/conversations/conv_1/archive",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/ai/conversations/conv_1/restore",
            "",
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/ai/applications/app_1/conversations/conv_1/artifacts",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/ai/applications/app_1/conversations/conv_1/artifacts",
            r#"{"kind":"note","title":null,"payload":{}}"#,
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/projects/1/ai/conversations?context_type=project&context_id=1",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/projects/1/ai/conversations",
            r#"{"context_type":"project","context_id":"1"}"#,
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/projects/1/ai/conversations/list",
            "",
            true,
        ),
        (
            "conversations",
            Method::GET,
            "/projects/1/ai/conversations/conv_1",
            "",
            true,
        ),
        (
            "conversations",
            Method::PATCH,
            "/projects/1/ai/conversations/conv_1",
            r#"{"title":"Renamed"}"#,
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/projects/1/ai/conversations/conv_1/stop",
            "",
            true,
        ),
        (
            "conversations",
            Method::POST,
            "/projects/1/ai/conversations/conv_1/archive",
            "",
            true,
        ),
        // messages
        (
            "messages",
            Method::POST,
            "/ai/conversations/conv_1/messages",
            r#"{"content":"hello"}"#,
            true,
        ),
        (
            "messages",
            Method::POST,
            "/projects/1/ai/conversations/conv_1/messages",
            r#"{"content":"hello"}"#,
            true,
        ),
        (
            "messages",
            Method::GET,
            "/ai/conversations/conv_1/stream",
            "",
            false,
        ),
        (
            "messages",
            Method::GET,
            "/projects/1/ai/conversations/conv_1/stream",
            "",
            false,
        ),
        // attachments
        (
            "attachments",
            Method::POST,
            "/ai/conversations/conv_1/attachments",
            "",
            false,
        ),
        (
            "attachments",
            Method::GET,
            "/ai/conversations/conv_1/attachments/att_1?name=notes.txt",
            "",
            true,
        ),
        // permissions
        (
            "permissions",
            Method::POST,
            "/ai/conversations/conv_1/permission-mode",
            r#"{"permission_mode":"ask"}"#,
            true,
        ),
        (
            "permissions",
            Method::POST,
            "/projects/1/ai/conversations/conv_1/permission-mode",
            r#"{"permission_mode":"ask"}"#,
            true,
        ),
        (
            "permissions",
            Method::POST,
            "/ai/conversations/conv_1/permissions/perm_1/resolve",
            r#"{"decision":{"type":"allow_tool"}}"#,
            true,
        ),
        (
            "permissions",
            Method::POST,
            "/projects/1/ai/conversations/conv_1/permissions/perm_1/resolve",
            r#"{"decision":{"type":"allow_tool"}}"#,
            true,
        ),
        // pending_actions
        (
            "pending_actions",
            Method::GET,
            "/ai/conversations/conv_1/pending-actions",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::GET,
            "/ai/pending-actions/act_1",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::POST,
            "/ai/pending-actions/act_1/confirm",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::POST,
            "/ai/pending-actions/act_1/reject",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::GET,
            "/projects/1/ai/conversations/conv_1/pending-actions",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::GET,
            "/projects/1/ai/pending-actions/act_1",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::POST,
            "/projects/1/ai/pending-actions/act_1/confirm",
            "",
            true,
        ),
        (
            "pending_actions",
            Method::POST,
            "/projects/1/ai/pending-actions/act_1/reject",
            "",
            true,
        ),
        // preview
        (
            "preview",
            Method::POST,
            "/ai/preview/authorize",
            r#"{"sandbox_public_id":"sbx_1","port":3000}"#,
            true,
        ),
        (
            "preview",
            Method::POST,
            "/ai/workspace/preview-link",
            r#"{"port":3000}"#,
            true,
        ),
        (
            "preview",
            Method::POST,
            "/ai/applications/app_1/preview-link",
            r#"{"port":3000}"#,
            true,
        ),
        // readiness
        (
            "readiness",
            Method::GET,
            "/projects/1/ai/readiness",
            "",
            true,
        ),
    ]
}

// Every handler group answers 401 without credentials, before touching the
// database or the audit log.
#[tokio::test]
async fn every_handler_group_requires_authentication() {
    for (group, method, uri, body, _) in handler_group_routes() {
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));

        let status = send(&harness, method.clone(), uri, body, None).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{group}: {method} {uri}");
        assert!(
            statements.is_empty(),
            "{group}: {method} {uri}: {statements:?}"
        );
        assert_eq!(audits, 0, "{group}: {method} {uri}");
    }
}

// A key that holds no permission is refused by every handler group's guard
// (403) before any lookup, write or audit entry.
#[tokio::test]
async fn every_handler_group_refuses_a_key_without_permissions() {
    for (group, method, uri, body, guarded_by_handler) in handler_group_routes() {
        if !guarded_by_handler {
            continue;
        }
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));

        let status = send(
            &harness,
            method.clone(),
            uri,
            body,
            Some(auth_of(1, None, Some(Vec::new()))),
        )
        .await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::FORBIDDEN, "{group}: {method} {uri}");
        assert!(
            statements.is_empty(),
            "{group}: {method} {uri}: {statements:?}"
        );
        assert_eq!(audits, 0, "{group}: {method} {uri}");
    }
}

// Deployment tokens never own conversations, applications or workspaces:
// every handler group refuses one before any lookup, write or audit entry.
#[tokio::test]
async fn every_handler_group_refuses_deployment_tokens() {
    for (group, method, uri, body, guarded_by_handler) in handler_group_routes() {
        if !guarded_by_handler {
            continue;
        }
        let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));
        let token = AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "deploy".to_string(),
            vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
        );

        let status = send(&harness, method.clone(), uri, body, Some(token)).await;

        let (statements, audits) = harness.finish();
        assert_eq!(status, StatusCode::FORBIDDEN, "{group}: {method} {uri}");
        assert!(
            statements.is_empty(),
            "{group}: {method} {uri}: {statements:?}"
        );
        assert_eq!(audits, 0, "{group}: {method} {uri}");
    }
}

// The sandbox MCP bridge is authenticated by its per-turn bearer, not by a
// session: an unknown bridge or a missing bearer is a 401 that reveals
// nothing about which bridges exist.
#[tokio::test]
async fn sandbox_mcp_bridge_refuses_requests_without_its_bearer() {
    let harness = Harness::new(MockDatabase::new(DatabaseBackend::Postgres));

    let status = send(
        &harness,
        Method::POST,
        "/ai/sandbox-tools/bridge_1/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        None,
    )
    .await;

    let (statements, audits) = harness.finish();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(statements.is_empty(), "{statements:?}");
    assert_eq!(audits, 0);
}
