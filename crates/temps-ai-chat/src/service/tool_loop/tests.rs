// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Characterization of a whole turn: what the client sees, in what order, and
//! what is persisted, for each way a turn can end.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement};
use temps_ai::{
    AiError, AiRequest, AiResponse, ChatTurnStream, ContextUsageSource, ContextWindowUsage,
    TokenStream, TurnServices,
};

use super::*;
use crate::service::tests::{
    collect_all, db_service_from_arc, echo_tools, owner_admin_grant, owner_admin_role, test_auth,
    test_conversation, test_request_metadata, StubProvider,
};

const WITHHELD: &str = "Tool completed; detailed result is withheld from the chat transcript.";

type CollectedEvents = Vec<Result<ChatStreamEvent, ChatError>>;
type TurnEventStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>;

/// One scripted provider call.
enum Round {
    /// The call itself fails, before any stream exists.
    OpenErr(AiError),
    /// The call streams these items, then ends.
    Items(Vec<Result<ChatStreamDelta, AiError>>),
    /// The call builds its own stream from the services it was handed.
    Custom(Box<dyn FnOnce(TurnServices) -> ChatTurnStream + Send>),
}

fn deltas(items: Vec<ChatStreamDelta>) -> Round {
    Round::Items(items.into_iter().map(Ok).collect())
}

/// An `AiService` driven round by round, including mid-stream failures.
struct TurnScript {
    rounds: Mutex<VecDeque<Round>>,
    requests: Arc<Mutex<Vec<ChatTurnRequest>>>,
    advance_per_call: Option<Duration>,
}

#[async_trait]
impl AiService for TurnScript {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn_with_services(
        &self,
        request: ChatTurnRequest,
        services: TurnServices,
    ) -> Result<ChatTurnStream, AiError> {
        self.requests.lock().unwrap().push(request);
        if let Some(by) = self.advance_per_call {
            tokio::time::advance(by).await;
        }
        let round = self.rounds.lock().unwrap().pop_front();
        match round {
            Some(Round::OpenErr(error)) => Err(error),
            Some(Round::Items(items)) => Ok(Box::pin(futures::stream::iter(items))),
            Some(Round::Custom(build)) => Ok(build(services)),
            None => Err(AiError::Provider {
                purpose: "test".to_string(),
                reason: "the script has no more rounds".to_string(),
            }),
        }
    }
}

/// A service, its mock database, and the requests its provider received.
struct Fixture {
    svc: ConversationService,
    db: Arc<DatabaseConnection>,
    requests: Arc<Mutex<Vec<ChatTurnRequest>>>,
    auth: AuthContext,
    tool_calls: Arc<std::sync::atomic::AtomicUsize>,
    provider: Arc<dyn ConversationContextProvider>,
}

impl Fixture {
    fn new(rounds: Vec<Round>, db: MockDatabase) -> Self {
        Self::with_clock(rounds, db, None)
    }

    fn with_clock(rounds: Vec<Round>, db: MockDatabase, advance: Option<Duration>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let db = Arc::new(db.into_connection());
        let mut svc = db_service_from_arc(db.clone());
        svc.ai = Arc::new(TurnScript {
            rounds: Mutex::new(rounds.into_iter().collect()),
            requests: requests.clone(),
            advance_per_call: advance,
        });
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
            tool_calls: tool_calls.clone(),
        });
        Self {
            svc,
            db,
            requests,
            auth: api_key_auth(),
            tool_calls,
            provider,
        }
    }

    fn tool_runs(&self) -> usize {
        self.tool_calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<ChatTurnRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Every statement the turn ran, once its tasks have let go of the database.
    async fn into_log(self) -> Vec<Statement> {
        let Self { svc, mut db, .. } = self;
        drop(svc);
        loop {
            match Arc::try_unwrap(db) {
                Ok(db) => {
                    return db
                        .into_transaction_log()
                        .into_iter()
                        .flat_map(|transaction| transaction.statements().to_vec())
                        .collect();
                }
                Err(shared) => {
                    db = shared;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }

    async fn start(&self, conv: &ai_conversations::Model, opts: TurnOptions) -> TurnEventStream {
        self.svc
            .try_tool_loop_in_workspace(
                conv,
                vec![],
                Some(self.provider.clone()),
                echo_tools(),
                &self.auth,
                &test_request_metadata(),
                None,
                Vec::new(),
                opts.workspace.then(|| temps_ai::HarnessWorkspace {
                    sandbox_label: "app_tool_loop".to_string(),
                    host_work_dir: std::path::PathBuf::from("/tmp/app_tool_loop"),
                }),
                temps_ai::SensitiveEnvironment::default(),
                Vec::new(),
                opts.turn_id,
                false,
                opts.draft,
            )
            .await
    }

    async fn run(&self, conv: &ai_conversations::Model, opts: TurnOptions) -> CollectedEvents {
        collect_all(self.start(conv, opts).await).await
    }
}

#[derive(Default)]
struct TurnOptions {
    workspace: bool,
    turn_id: Option<String>,
    draft: Option<i64>,
}

fn harness() -> TurnOptions {
    TurnOptions {
        workspace: true,
        ..Default::default()
    }
}

fn harness_conversation() -> ai_conversations::Model {
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    conversation
}

fn assistant_conversation() -> ai_conversations::Model {
    let mut conversation = test_conversation();
    conversation.context_type = crate::providers::assistant::ASSISTANT_CONTEXT_TYPE.to_string();
    conversation
}

/// A conversation whose durable row says this exact turn owns it.
fn claimed(mut conversation: ai_conversations::Model, turn_id: &str) -> ai_conversations::Model {
    conversation.turn_status = "running".to_string();
    conversation.active_turn_id = Some(turn_id.to_string());
    conversation
}

fn assistant_message_row() -> ai_messages::Model {
    ai_messages::Model {
        id: 1,
        conversation_id: 1,
        role: "assistant".to_string(),
        content: String::new(),
        metadata: None,
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: Utc::now(),
    }
}

fn api_key_row() -> temps_entities::api_keys::Model {
    let now = Utc::now();
    temps_entities::api_keys::Model {
        id: 73,
        name: "tool-loop".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: test_auth().user.expect("test user").id,
        role_type: "admin".to_string(),
        permissions: None,
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    }
}

fn api_key_auth() -> AuthContext {
    let key = api_key_row();
    AuthContext::new_api_key(
        test_auth().user.expect("test user"),
        Some(Role::Admin),
        None,
        key.name,
        key.id,
    )
}

/// Mock rows for `refreshes` successful authorization refreshes. Updates
/// succeed; `MockDatabase` keeps queries and executes in separate queues.
fn db_with_refreshes(refreshes: usize) -> MockDatabase {
    let mut db = MockDatabase::new(DatabaseBackend::Postgres).append_exec_results(
        (0..64)
            .map(|_| MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
            .collect::<Vec<_>>(),
    );
    for _ in 0..refreshes {
        db = db
            .append_query_results([[api_key_row()]])
            .append_query_results([[test_auth().user.expect("test user")]])
            .append_query_results([[owner_admin_role()]])
            .append_query_results([[owner_admin_grant()]]);
    }
    db
}

/// The next refresh after this finds the key revoked.
fn revoke_key(db: MockDatabase) -> MockDatabase {
    db.append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
}

fn writes_to<'a>(statements: &'a [Statement], prefix: &str) -> Vec<&'a Statement> {
    statements
        .iter()
        .filter(|statement| statement.sql.starts_with(prefix))
        .collect()
}

const MESSAGE_UPDATE: &str = "UPDATE \"ai_messages\"";
const MESSAGE_INSERT: &str = "INSERT INTO \"ai_messages\"";
const MESSAGE_DELETE: &str = "DELETE FROM \"ai_messages\"";
const CONVERSATION_UPDATE: &str = "UPDATE \"ai_conversations\"";

fn json_value(statement: &Statement) -> serde_json::Value {
    statement
        .values
        .as_ref()
        .and_then(|values| {
            values.0.iter().find_map(|value| match value {
                sea_orm::Value::Json(Some(json)) => Some(json.as_ref().clone()),
                _ => None,
            })
        })
        .unwrap_or(serde_json::Value::Null)
}

fn string_values(statement: &Statement) -> Vec<String> {
    statement
        .values
        .as_ref()
        .map(|values| {
            values
                .0
                .iter()
                .filter_map(|value| match value {
                    sea_orm::Value::String(Some(text)) => Some(text.as_ref().clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn part_kinds(metadata: &serde_json::Value) -> Vec<String> {
    metadata["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|part| part["type"].as_str().unwrap_or("?").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One line per client event, so a test can pin the exact sequence.
fn describe(events: &[Result<ChatStreamEvent, ChatError>]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            Ok(ChatStreamEvent::Token(text)) => format!("token:{text}"),
            Ok(ChatStreamEvent::ContextUsage(usage)) => format!("usage:{}", usage.used_tokens),
            Ok(ChatStreamEvent::ToolCall { id, name, .. }) => format!("call:{id}:{name}"),
            Ok(ChatStreamEvent::ToolResult { id, name, content }) => {
                format!("result:{id}:{name}:{content}")
            }
            Ok(ChatStreamEvent::PermissionRequested { id, tool_name, .. }) => {
                format!("permission:{id}:{tool_name}")
            }
            Err(error) => format!("error:{error}"),
        })
        .collect()
}

fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: arguments.to_string(),
    }
}

fn call_delta(id: &str, arguments: &str) -> ChatStreamDelta {
    ChatStreamDelta::ToolCall(tool_call(id, "echo", arguments))
}

fn native_call(id: &str) -> ChatStreamDelta {
    ChatStreamDelta::ToolCall(tool_call(id, "Bash", r#"{"command":"pwd"}"#))
}

fn text(value: &str) -> ChatStreamDelta {
    ChatStreamDelta::Text(value.to_string())
}

fn provider_error(reason: &str) -> AiError {
    AiError::Provider {
        purpose: "chat.test.tools".to_string(),
        reason: reason.to_string(),
    }
}

fn interrupted(reason: &str) -> AiError {
    AiError::StreamInterrupted {
        purpose: "chat.test.tools".to_string(),
        reason: reason.to_string(),
    }
}

fn retained(reason: &str) -> AiError {
    AiError::RetainedHarnessDiagnostic {
        purpose: "chat.test.tools".to_string(),
        reason: reason.to_string(),
    }
}

fn usage(used_tokens: u64) -> ChatStreamDelta {
    ChatStreamDelta::ContextUsage(ContextWindowUsage {
        used_tokens,
        limit_tokens: None,
        model: None,
        source: ContextUsageSource::ProviderReported,
        estimated: true,
    })
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Wait for the monitor's terminal event, which is published after it has
/// finished writing the turn's final status.
async fn wait_for_turn_complete(live: &mut broadcast::Receiver<WireEvent>) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut names = Vec::new();
        loop {
            let event = live.recv().await.expect("live event");
            names.push(event.event);
            if names.last().map(String::as_str) == Some("turn_complete") {
                return names;
            }
        }
    })
    .await
    .expect("the monitor must publish turn_complete")
}

fn approval_request(id: &str, input: serde_json::Value) -> PermissionRequest {
    PermissionRequest {
        id: id.to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: "Bash".to_string(),
        input,
    }
}

fn resolution(decision: PermissionDecision) -> PermissionResolution {
    PermissionResolution {
        decision,
        auth: test_auth(),
        metadata: test_request_metadata(),
    }
}

fn decision_label(decision: &Result<PermissionDecision, AiError>) -> String {
    match decision {
        Ok(PermissionDecision::AllowTool) => "allowed".to_string(),
        Ok(_) => "other decision".to_string(),
        Err(error) => format!("failed: {error}"),
    }
}

/// A provider stream that waits on one interaction and reports its outcome.
fn awaits_interaction(request: PermissionRequest) -> Round {
    Round::Custom(Box::new(move |services| {
        let interactions = services.interactions.expect("interaction executor");
        Box::pin(async_stream::stream! {
            let decision = interactions(request).await;
            yield Ok(ChatStreamDelta::Text(decision_label(&decision)));
        })
    }))
}

fn take_pending(svc: &ConversationService, id: &str) -> PendingPermissionEntry {
    svc.pending_permissions
        .lock()
        .unwrap()
        .remove(id)
        .expect("a registered permission")
}

// ---- Transcript and event order ------------------------------------------

#[tokio::test]
async fn prose_after_a_tool_round_is_separated_by_a_blank_line_and_persisted_in_order() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![text("Looking."), call_delta("c1", "{}")]),
            deltas(vec![text("Done.")]),
        ],
        db_with_refreshes(1),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(
        describe(&events),
        vec![
            "token:Looking.".to_string(),
            "call:c1:echo".to_string(),
            format!("result:c1:echo:{WITHHELD}"),
            "token:\n\n".to_string(),
            "token:Done.".to_string(),
        ]
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    let replay = &requests[1].messages;
    assert_eq!(replay[0].role, "assistant");
    assert_eq!(replay[0].content, "Looking.");
    assert_eq!(replay[0].tool_calls.as_ref().map(Vec::len), Some(1));
    assert_eq!(replay[1].role, "tool");
    assert_eq!(replay[1].content, "tool result");

    let log = fixture.into_log().await;
    let inserts = writes_to(&log, MESSAGE_INSERT);
    assert_eq!(inserts.len(), 1);
    assert!(string_values(inserts[0]).contains(&"Looking.\n\nDone.".to_string()));
    let metadata = json_value(inserts[0]);
    assert_eq!(part_kinds(&metadata), vec!["text", "tool", "text"]);
    assert_eq!(metadata["parts"][2]["text"], "\n\nDone.");
    assert_eq!(metadata["tools"][0]["result"], WITHHELD);
}

#[tokio::test]
async fn round_requests_carry_the_turn_configuration() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![call_delta("c1", "{}")]),
            deltas(vec![text("Done.")]),
        ],
        db_with_refreshes(1),
    );
    let mut conversation = test_conversation();
    conversation.ai_thinking_level = Some("high".to_string());

    fixture.run(&conversation, Default::default()).await;

    let requests = fixture.requests();
    for request in &requests {
        assert_eq!(request.purpose, "chat.test.tools");
        assert_eq!(request.conversation_id.as_deref(), Some("1"));
        assert_eq!(request.project_id, Some(7));
        assert_eq!(request.provider.as_deref(), Some("gateway"));
        assert_eq!(request.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(request.thinking_level.as_deref(), Some("high"));
        assert_eq!(request.permission_mode.as_deref(), Some("confirm-actions"));
        assert_eq!(request.tools.len(), 1);
        assert!(request.harness_mcp_server.is_none());
        assert!(!request.reset_retained_session);
        assert!(!request.capture_session_title);
    }
}

#[tokio::test]
async fn draft_is_checkpointed_on_each_structural_event_then_finalized() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![text("Hi"), call_delta("c1", "{}")]),
            deltas(vec![text("there")]),
        ],
        db_with_refreshes(1),
    );

    let events = fixture
        .run(
            &test_conversation(),
            TurnOptions {
                draft: Some(99),
                ..Default::default()
            },
        )
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let log = fixture.into_log().await;
    assert!(writes_to(&log, MESSAGE_INSERT).is_empty());
    let updates = writes_to(&log, MESSAGE_UPDATE);
    assert!(updates.len() >= 4, "{updates:?}");

    // The first text delta checkpoints immediately, with the text still open.
    let first = json_value(updates[0]);
    assert_eq!(first["draft"], true);
    assert_eq!(part_kinds(&first), vec!["text"]);
    assert!(first.get("tools").is_none());
    // The tool call closes the text part and has no result yet.
    let second = json_value(updates[1]);
    assert_eq!(second["draft"], true);
    assert_eq!(part_kinds(&second), vec!["text", "tool"]);
    assert!(second["tools"][0]["result"].is_null());
    // Its result follows once the tool has run.
    let third = json_value(updates[2]);
    assert_eq!(third["draft"], true);
    assert_eq!(third["tools"][0]["result"], WITHHELD);

    let last = updates.last().expect("final update");
    let finished = json_value(last);
    assert!(finished.get("draft").is_none());
    assert_eq!(part_kinds(&finished), vec!["text", "tool", "text"]);
    assert!(string_values(last).contains(&"Hi\n\nthere".to_string()));
}

#[tokio::test]
async fn empty_turn_removes_its_draft_and_records_the_failure() {
    let db = db_with_refreshes(0).append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(vec![Round::OpenErr(provider_error("boom"))], db);

    let events = fixture
        .run(
            &test_conversation(),
            TurnOptions {
                draft: Some(99),
                ..Default::default()
            },
        )
        .await;

    let described = describe(&events);
    assert_eq!(described.len(), 1, "{described:?}");
    assert!(
        described[0].contains("no reply was produced."),
        "{described:?}"
    );
    assert!(described[0].contains("boom"), "{described:?}");
    assert!(
        described[0].contains("Review the concrete error and the selected harness/provider status"),
        "{described:?}"
    );

    let log = fixture.into_log().await;
    assert!(writes_to(&log, MESSAGE_UPDATE).is_empty());
    assert_eq!(writes_to(&log, MESSAGE_DELETE).len(), 1);
    let failures = writes_to(&log, CONVERSATION_UPDATE);
    assert_eq!(failures.len(), 1);
    assert!(json_value(failures[0]).get("last_failure").is_some());
}

#[tokio::test]
async fn unbacked_proposal_claim_is_replaced_in_the_persisted_message() {
    let db = db_with_refreshes(0).append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(
        vec![deltas(vec![text(
            "Proposal staged — not executed. Please confirm it in the UI.",
        )])],
        db,
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert!(matches!(
        events.last(),
        Some(Err(ChatError::ProposalNotStaged))
    ));
    let log = fixture.into_log().await;
    let inserts = writes_to(&log, MESSAGE_INSERT);
    let persisted = string_values(inserts[0]).join("\n");
    assert!(persisted.contains("Temps could not verify this proposal"));
    assert!(!persisted.contains("Proposal staged"));
    assert_eq!(part_kinds(&json_value(inserts[0])), vec!["text"]);
}

#[tokio::test]
async fn context_usage_is_persisted_and_published() {
    let db = db_with_refreshes(0).append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(vec![deltas(vec![usage(42), text("Hi")])], db);

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(describe(&events), vec!["usage:42", "token:Hi"]);
    let log = fixture.into_log().await;
    let updates = writes_to(&log, CONVERSATION_UPDATE);
    assert_eq!(updates.len(), 1);
    assert_eq!(json_value(updates[0])["context_usage"]["used_tokens"], 42);
}

#[tokio::test]
async fn provider_session_id_is_kept_when_later_metadata_omits_it() {
    let fixture = Fixture::new(
        vec![deltas(vec![
            ChatStreamDelta::SessionMetadata {
                session_id: Some("session-1".to_string()),
                title: None,
            },
            ChatStreamDelta::SessionMetadata {
                session_id: None,
                title: None,
            },
            text("Hi"),
        ])],
        db_with_refreshes(0),
    );

    fixture.run(&test_conversation(), Default::default()).await;

    let log = fixture.into_log().await;
    let updates = writes_to(&log, CONVERSATION_UPDATE);
    assert_eq!(updates.len(), 1);
    assert!(updates[0].sql.contains("cli_session_id"));
    assert!(string_values(updates[0]).contains(&"session-1".to_string()));
}

#[tokio::test]
async fn permission_delta_is_forwarded_after_checkpointing_the_draft() {
    let fixture = Fixture::new(
        vec![deltas(vec![ChatStreamDelta::PermissionRequested(
            approval_request("perm-1", serde_json::json!({})),
        )])],
        db_with_refreshes(0),
    );

    let events = fixture
        .run(
            &test_conversation(),
            TurnOptions {
                draft: Some(99),
                ..Default::default()
            },
        )
        .await;

    // Forwarded, not answered: a round with no calls still ends the turn, and
    // nothing was said, so it fails as empty.
    let described = describe(&events);
    assert_eq!(described[0], "permission:perm-1:Bash");
    assert!(
        described[1].contains("no reply was produced"),
        "{described:?}"
    );
    let log = fixture.into_log().await;
    assert_eq!(writes_to(&log, MESSAGE_UPDATE).len(), 1);
    assert_eq!(writes_to(&log, MESSAGE_DELETE).len(), 1);
}

// ---- Budget --------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn deadline_stop_states_the_reason_before_the_tool_free_salvage_answer() {
    let fixture = Fixture::with_clock(
        vec![
            deltas(vec![call_delta("c1", "{}")]),
            deltas(vec![text("From what I gathered.")]),
        ],
        db_with_refreshes(1),
        Some(Duration::from_secs(16 * 60)),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:c1:echo".to_string(),
            format!("result:c1:echo:{WITHHELD}"),
            "token:\n\n_The assistant reached the time limit for a single turn (configurable in \
             Settings → AI). What follows is based only on what it gathered so far — ask it to \
             continue if that looks incomplete._\n\n"
                .to_string(),
            "token:From what I gathered.".to_string(),
        ]
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    let salvage = &requests[1];
    assert_eq!(salvage.purpose, "chat.test.tools.final");
    assert!(salvage.tools.is_empty());
    assert!(salvage.harness_mcp_server.is_none());
    let directive = salvage.messages.last().expect("final directive");
    assert_eq!(directive.role, "user");
    assert_eq!(
        directive.content,
        "You have no more tool calls available. Using ONLY the tool results above, write your \
         final answer to my request now, in plain prose. Do not emit tool-call JSON or describe \
         tools you would call. If the data is insufficient, briefly state what you found and \
         what is still missing."
    );
}

#[tokio::test]
async fn round_backstop_says_the_run_was_long_and_warns_the_model_before_it() {
    let mut rounds: Vec<Round> = (0..ASSISTANT_MAX_ROUNDS)
        .map(|i| {
            deltas(vec![call_delta(
                &format!("c{i}"),
                &format!("{{\"n\":{i}}}"),
            )])
        })
        .collect();
    rounds.push(deltas(vec![text("Wrapped up.")]));
    let fixture = Fixture::new(rounds, db_with_refreshes(ASSISTANT_MAX_ROUNDS));

    let events = fixture
        .run(&assistant_conversation(), Default::default())
        .await;

    let requests = fixture.requests();
    assert_eq!(requests.len(), ASSISTANT_MAX_ROUNDS + 1);
    assert_eq!(fixture.tool_runs(), ASSISTANT_MAX_ROUNDS);
    let last_message = |index: usize| requests[index].messages.last().expect("message").clone();
    let warned_after = ASSISTANT_MAX_ROUNDS - 3;
    assert!(!last_message(warned_after - 1)
        .content
        .contains(STEPS_LEFT_NOTE));
    assert!(last_message(warned_after).content.contains(STEPS_LEFT_NOTE));
    assert_eq!(
        requests.last().expect("salvage").purpose,
        "chat.assistant.tools.final"
    );

    let described = describe(&events);
    let tail = &described[described.len() - 2..];
    assert!(
        tail[0].contains("_The assistant stopped after an unusually long run of steps."),
        "{tail:?}"
    );
    assert_eq!(tail[1], "token:Wrapped up.");
}

#[tokio::test]
async fn three_rejected_rounds_in_a_row_stop_the_turn() {
    let same = "{\"n\":1}";
    let fixture = Fixture::new(
        vec![
            deltas(vec![call_delta("c0", same)]),
            deltas(vec![call_delta("c1", same)]),
            deltas(vec![call_delta("c2", same)]),
            deltas(vec![call_delta("c3", same)]),
            deltas(vec![text("Stopping here.")]),
        ],
        db_with_refreshes(4),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    let requests = fixture.requests();
    assert_eq!(
        requests.len(),
        5,
        "one productive, three rejected, one salvage"
    );
    assert_eq!(fixture.tool_runs(), 1);
    assert_eq!(requests[4].purpose, "chat.test.tools.final");
    let described = describe(&events);
    assert!(
        described[described.len() - 2]
            .contains("stopped after several attempts in a row produced only errors"),
        "{described:?}"
    );
    assert_eq!(described[described.len() - 1], "token:Stopping here.");
}

#[tokio::test]
async fn a_productive_round_resets_the_rejection_streak() {
    let one = "{\"n\":1}";
    let two = "{\"n\":2}";
    let fixture = Fixture::new(
        vec![
            deltas(vec![call_delta("c0", one)]),
            deltas(vec![call_delta("c1", one)]),
            deltas(vec![call_delta("c2", one)]),
            deltas(vec![call_delta("c3", two)]),
            deltas(vec![call_delta("c4", two)]),
            deltas(vec![call_delta("c5", two)]),
            deltas(vec![text("Finished.")]),
        ],
        db_with_refreshes(6),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    let requests = fixture.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(requests[6].purpose, "chat.test.tools");
    assert_eq!(fixture.tool_runs(), 2);
    assert!(!describe(&events)
        .iter()
        .any(|event| event.contains("The assistant")));
}

#[tokio::test]
async fn calls_past_the_per_round_cap_are_answered_but_not_run() {
    let calls = (0..ASSISTANT_MAX_CALLS_PER_ROUND + 2)
        .map(|i| call_delta(&format!("c{i}"), &format!("{{\"n\":{i}}}")))
        .collect();
    let fixture = Fixture::new(
        vec![deltas(calls), deltas(vec![text("Done.")])],
        db_with_refreshes(ASSISTANT_MAX_CALLS_PER_ROUND),
    );

    let events = fixture
        .run(&assistant_conversation(), Default::default())
        .await;

    assert_eq!(fixture.tool_runs(), ASSISTANT_MAX_CALLS_PER_ROUND);
    let requests = fixture.requests();
    let answers: Vec<&ChatMessage> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect();
    assert_eq!(answers.len(), ASSISTANT_MAX_CALLS_PER_ROUND + 2);
    assert_eq!(
        answers[ASSISTANT_MAX_CALLS_PER_ROUND - 1].content,
        "tool result"
    );
    for answer in &answers[ASSISTANT_MAX_CALLS_PER_ROUND..] {
        assert_eq!(
            answer.content,
            tool_call_cap_message(ASSISTANT_MAX_CALLS_PER_ROUND)
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ChatStreamEvent::ToolResult { .. })))
            .count(),
        ASSISTANT_MAX_CALLS_PER_ROUND + 2
    );
}

struct LargeResultProvider;

#[async_trait]
impl ConversationContextProvider for LargeResultProvider {
    fn context_type(&self) -> &'static str {
        "test"
    }
    async fn seed(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
    ) -> Option<crate::provider::ConversationSeed> {
        None
    }
    async fn tools(&self, _project_id: Option<i32>, _context_id: &str) -> Vec<ChatTool> {
        echo_tools()
    }
    async fn execute_tool(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
        _name: &str,
        _arguments: &str,
    ) -> String {
        "x".repeat(100 * 1024)
    }
}

#[tokio::test]
async fn carried_tool_results_are_trimmed_before_the_next_round() {
    let mut fixture = Fixture::new(
        vec![
            deltas(vec![call_delta("c0", "{\"n\":0}")]),
            deltas(vec![call_delta("c1", "{\"n\":1}")]),
            deltas(vec![call_delta("c2", "{\"n\":2}")]),
            deltas(vec![text("Done.")]),
        ],
        db_with_refreshes(3),
    );
    fixture.provider = Arc::new(LargeResultProvider);

    fixture.run(&test_conversation(), Default::default()).await;

    let requests = fixture.requests();
    let carried: usize = requests[3]
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .map(|message| message.content.len())
        .sum();
    assert!(carried <= 192 * 1024, "carried {carried} bytes");
    let newest = requests[3].messages.last().expect("newest result");
    assert_eq!(
        newest.content.len(),
        100 * 1024,
        "the newest result is intact"
    );
}

// ---- Tool errors and interrupted rounds ----------------------------------

#[tokio::test]
async fn a_tool_call_before_a_stream_error_still_runs_on_the_gateway_path() {
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(call_delta("c1", "{}")),
                Err(provider_error("blip")),
            ]),
            deltas(vec![text("Recovered.")]),
        ],
        db_with_refreshes(1),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:c1:echo".to_string(),
            format!("result:c1:echo:{WITHHELD}"),
            "token:Recovered.".to_string(),
        ]
    );
    assert_eq!(fixture.tool_runs(), 1);
}

#[tokio::test]
async fn a_stream_error_without_tool_calls_keeps_the_partial_answer() {
    let fixture = Fixture::new(
        vec![Round::Items(vec![
            Ok(text("Partial")),
            Err(provider_error("blip")),
        ])],
        db_with_refreshes(0),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(describe(&events), vec!["token:Partial"]);
    assert_eq!(fixture.requests().len(), 1, "no salvage call");
}

#[tokio::test]
async fn an_interrupted_round_never_runs_its_tool_calls() {
    // One row for the message insert to return, one for the failure write.
    let db = db_with_refreshes(0)
        .append_query_results([[assistant_message_row()]])
        .append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(
        vec![Round::Items(vec![
            Ok(call_delta("c1", "{}")),
            Err(interrupted("connection dropped")),
        ])],
        db,
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(fixture.tool_runs(), 0);
    assert_eq!(fixture.requests().len(), 1, "no salvage after interruption");
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(events[0], Ok(ChatStreamEvent::ToolCall { .. })));
    assert!(matches!(
        events[1],
        Err(ChatError::StreamInterrupted { .. })
    ));
    assert_eq!(
        describe(&events)[2],
        format!("result:c1:echo:{INTERRUPTED_TOOL_RESULT}")
    );

    let log = fixture.into_log().await;
    let metadata = json_value(writes_to(&log, MESSAGE_INSERT)[0]);
    assert_eq!(metadata["interrupted"], true);
    assert_eq!(metadata["tools"][0]["result"], INTERRUPTED_TOOL_RESULT);
    let failures = writes_to(&log, CONVERSATION_UPDATE);
    assert_eq!(failures.len(), 1);
    assert!(json_value(failures[0]).get("last_failure").is_some());
}

#[tokio::test]
async fn a_retained_diagnostic_fails_an_empty_turn_with_its_own_error() {
    let db = db_with_refreshes(0).append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(vec![Round::OpenErr(retained("harness exited"))], db);

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0],
        Err(ChatError::RetainedHarnessDiagnostic { reason, .. }) if reason == "harness exited"
    ));
}

#[tokio::test]
async fn a_retained_diagnostic_after_prose_is_reported_once_after_the_text() {
    let db = db_with_refreshes(0).append_query_results([[test_conversation()]]);
    let fixture = Fixture::new(
        vec![Round::Items(vec![
            Ok(text("Partial")),
            Err(retained("harness exited")),
        ])],
        db,
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(&events[0], Ok(ChatStreamEvent::Token(_))));
    assert!(matches!(
        &events[1],
        Err(ChatError::RetainedHarnessDiagnostic { .. })
    ));
}

// ---- Salvage -------------------------------------------------------------

#[tokio::test]
async fn salvage_answer_is_separated_from_earlier_prose_only_on_the_wire() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![text("Looking"), call_delta("c1", "{}")]),
            Round::OpenErr(provider_error("rate limited")),
            deltas(vec![text("Summary")]),
        ],
        db_with_refreshes(1),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    assert_eq!(
        describe(&events),
        vec![
            "token:Looking".to_string(),
            "call:c1:echo".to_string(),
            format!("result:c1:echo:{WITHHELD}"),
            "token:\n\n".to_string(),
            "token:Summary".to_string(),
        ]
    );
    assert_eq!(fixture.requests()[2].purpose, "chat.test.tools.final");
    let log = fixture.into_log().await;
    let insert = writes_to(&log, MESSAGE_INSERT)[0];
    assert!(string_values(insert).contains(&"Looking\n\nSummary".to_string()));
    let metadata = json_value(insert);
    assert_eq!(part_kinds(&metadata), vec!["text", "tool", "text"]);
    assert_eq!(metadata["parts"][2]["text"], "Summary");
}

#[tokio::test]
async fn a_native_round_without_prose_is_answered_by_a_salvage_call() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![
                native_call("n1"),
                ChatStreamDelta::ToolResult {
                    call: tool_call("n1", "Bash", r#"{"command":"pwd"}"#),
                    result: "/home/temps".to_string(),
                },
            ]),
            deltas(vec![text("Summary")]),
        ],
        db_with_refreshes(2),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:n1:Bash",
            "result:n1:Bash:/home/temps",
            "token:Summary",
        ]
    );
    assert_eq!(
        fixture.tool_runs(),
        0,
        "a native tool is never re-dispatched"
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].harness_mcp_server.is_some());
    assert!(requests[0].reset_retained_session);
    assert_eq!(requests[1].purpose, "chat.application.tools.final");
    assert!(requests[1].harness_mcp_server.is_none());
    assert!(!requests[1].reset_retained_session);
}

#[tokio::test]
async fn a_native_round_with_prose_is_the_final_answer() {
    let fixture = Fixture::new(
        vec![deltas(vec![native_call("n1"), text("All done.")])],
        db_with_refreshes(1),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(describe(&events), vec!["call:n1:Bash", "token:All done."]);
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn a_failed_salvage_call_closes_unresolved_tools_without_an_error_event() {
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(native_call("n1")),
                Err(provider_error("bridge down")),
            ]),
            Round::OpenErr(provider_error("still down")),
        ],
        db_with_refreshes(2),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:n1:Bash".to_string(),
            format!("result:n1:Bash:{INTERRUPTED_TOOL_RESULT}"),
        ]
    );
}

#[tokio::test]
async fn a_salvage_stream_error_keeps_what_arrived_without_an_error_event() {
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(native_call("n1")),
                Err(provider_error("bridge down")),
            ]),
            Round::Items(vec![Ok(text("Part")), Err(provider_error("still down"))]),
        ],
        db_with_refreshes(2),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:n1:Bash".to_string(),
            "token:Part".to_string(),
            format!("result:n1:Bash:{INTERRUPTED_TOOL_RESULT}"),
        ]
    );
}

#[tokio::test]
async fn an_interrupted_salvage_stream_fails_the_turn() {
    let db = db_with_refreshes(2).append_query_results([[harness_conversation()]]);
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(native_call("n1")),
                Err(provider_error("bridge down")),
            ]),
            Round::Items(vec![
                Ok(text("Part")),
                Err(interrupted("connection dropped")),
            ]),
        ],
        db,
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(describe(&events)[1], "token:Part");
    assert!(matches!(
        &events[2],
        Err(ChatError::StreamInterrupted { partial_bytes: 4, cause, .. })
            if cause == "connection dropped"
    ));
    assert_eq!(
        describe(&events)[3],
        format!("result:n1:Bash:{INTERRUPTED_TOOL_RESULT}")
    );
}

#[tokio::test]
async fn salvage_uses_only_text_and_context_usage() {
    let db = db_with_refreshes(2).append_query_results([[harness_conversation()]]);
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(native_call("n1")),
                Err(provider_error("bridge down")),
            ]),
            deltas(vec![
                call_delta("ignored", "{}"),
                ChatStreamDelta::SessionMetadata {
                    session_id: Some("ignored-session".to_string()),
                    title: None,
                },
                usage(7),
                text("Summary"),
            ]),
        ],
        db,
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(
        describe(&events),
        vec![
            "call:n1:Bash".to_string(),
            "usage:7".to_string(),
            "token:Summary".to_string(),
            format!("result:n1:Bash:{INTERRUPTED_TOOL_RESULT}"),
        ]
    );
    let log = fixture.into_log().await;
    assert!(!log.iter().any(|statement| string_values(statement)
        .iter()
        .any(|value| value == "ignored-session")));
}

#[tokio::test]
async fn authorization_revoked_before_salvage_stops_without_asking_the_model() {
    let fixture = Fixture::new(
        vec![Round::Items(vec![
            Ok(native_call("n1")),
            Err(provider_error("bridge down")),
        ])],
        revoke_key(db_with_refreshes(1)),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(
        &events[1],
        Err(ChatError::AuthorizationRefresh(
            ToolAuthorizationRefreshError::PrincipalInactive
        ))
    ));
    assert_eq!(fixture.requests().len(), 1);
    assert!(writes_to(&fixture.into_log().await, MESSAGE_INSERT).is_empty());
}

#[tokio::test]
async fn authorization_revoked_during_salvage_stops_the_stream() {
    let fixture = Fixture::new(
        vec![
            Round::Items(vec![
                Ok(native_call("n1")),
                Err(provider_error("bridge down")),
            ]),
            Round::Custom(Box::new(|_| Box::pin(futures::stream::pending()))),
        ],
        revoke_key(db_with_refreshes(2)),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(
        &events[1],
        Err(ChatError::AuthorizationRefresh(
            ToolAuthorizationRefreshError::PrincipalInactive
        ))
    ));
    assert_eq!(fixture.requests().len(), 2);
}

// ---- Managed processes ---------------------------------------------------

/// The event channel of the one MCP bridge this turn registered.
fn mcp_event_sender(
    registry: &Arc<Mutex<HashMap<String, HarnessMcpEntry>>>,
) -> tokio::sync::mpsc::Sender<ChatStreamDelta> {
    registry
        .lock()
        .unwrap()
        .values()
        .next()
        .expect("the turn registered its MCP bridge")
        .event_tx
        .clone()
}

fn process_call(id: &str) -> ToolCall {
    tool_call(id, PROCESS_START_TOOL, r#"{"name":"web"}"#)
}

#[tokio::test]
async fn provider_echoes_of_managed_process_tools_are_dropped() {
    let fixture = Fixture::new(
        vec![deltas(vec![
            ChatStreamDelta::ToolCall(process_call("p1")),
            ChatStreamDelta::ToolResult {
                call: tool_call("p1", "mcp__temps-chat__temps_process_start", "{}"),
                result: "echo".to_string(),
            },
            text("Started."),
        ])],
        db_with_refreshes(1),
    );

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(describe(&events), vec!["token:Started."]);
}

#[tokio::test]
async fn a_round_waits_for_the_terminal_event_of_a_managed_process_call() {
    let mut fixture = Fixture::new(vec![], db_with_refreshes(5));
    let registry = fixture.svc.harness_mcp_entries.clone();
    fixture.svc.ai = Arc::new(TurnScript {
        rounds: Mutex::new(VecDeque::from([Round::Custom(Box::new(move |_| {
            let events = mcp_event_sender(&registry);
            Box::pin(async_stream::stream! {
                events.send(ChatStreamDelta::ToolCall(process_call("p1"))).await.unwrap();
                yield Ok(ChatStreamDelta::Text("Starting.".to_string()));
                // The provider's stream ends here; the bridge reports later.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let result = ChatStreamDelta::ToolResult {
                        call: process_call("p1"),
                        result: r#"{"state":"running"}"#.to_string(),
                    };
                    let _ = events.send(result).await;
                });
            })
        }))])),
        requests: fixture.requests.clone(),
        advance_per_call: None,
    });

    let events = fixture.run(&harness_conversation(), harness()).await;

    let name = "mcp__temps-chat__temps_process_start";
    assert_eq!(
        describe(&events),
        vec![
            "token:Starting.".to_string(),
            format!("call:p1:{name}"),
            format!("result:p1:{name}:{{\"state\":\"running\"}}"),
        ]
    );
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_stops_waiting_for_a_managed_process_result() {
    let mut fixture = Fixture::new(vec![], db_with_refreshes(5));
    let registry = fixture.svc.harness_mcp_entries.clone();
    fixture.svc.ai = Arc::new(TurnScript {
        rounds: Mutex::new(VecDeque::from([Round::Custom(Box::new(move |_| {
            let events = mcp_event_sender(&registry);
            Box::pin(async_stream::stream! {
                events.send(ChatStreamDelta::ToolCall(process_call("p1"))).await.unwrap();
                yield Ok(ChatStreamDelta::Text("Starting.".to_string()));
            })
        }))])),
        requests: fixture.requests.clone(),
        advance_per_call: Some(Duration::from_secs(16 * 60)),
    });

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(
        describe(&events),
        vec![
            "token:Starting.".to_string(),
            "call:p1:mcp__temps-chat__temps_process_start".to_string(),
        ],
        "the prose stands as the answer, with no failure and no time-limit note"
    );
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn a_closed_mcp_bridge_does_not_end_the_round() {
    let mut fixture = Fixture::new(vec![], db_with_refreshes(2));
    let registry = fixture.svc.harness_mcp_entries.clone();
    fixture.svc.ai = Arc::new(TurnScript {
        rounds: Mutex::new(VecDeque::from([Round::Custom(Box::new(move |_| {
            registry.lock().unwrap().clear();
            Box::pin(async_stream::stream! {
                tokio::task::yield_now().await;
                yield Ok(ChatStreamDelta::Text("Still here.".to_string()));
            })
        }))])),
        requests: fixture.requests.clone(),
        advance_per_call: None,
    });

    let events = fixture.run(&harness_conversation(), harness()).await;

    assert_eq!(describe(&events), vec!["token:Still here."]);
}

// ---- Interactions --------------------------------------------------------

#[tokio::test]
async fn provider_approval_waits_in_the_registry_with_a_redacted_copy() {
    let fixture = Fixture::new(
        vec![awaits_interaction(approval_request(
            "perm-1",
            serde_json::json!({ "api_key": "hunter2", "command": "pwd" }),
        ))],
        db_with_refreshes(0),
    );

    let stream = fixture
        .start(&test_conversation(), Default::default())
        .await;
    let pending = fixture.svc.pending_permissions.clone();
    eventually("the provider's approval to be registered", || {
        pending.lock().unwrap().contains_key("perm-1")
    })
    .await;

    let entry = take_pending(&fixture.svc, "perm-1");
    assert_eq!(entry.conv_public_id, "pub1");
    assert_eq!(entry.origin, PendingPermissionOrigin::Provider);
    assert_eq!(entry.kind, PermissionKind::ToolApproval);
    assert_eq!(entry.tool_name, "Bash");
    assert!(!entry.input.to_string().contains("hunter2"));
    assert_eq!(entry.input["command"], "pwd");
    assert!(entry
        .sender
        .send(resolution(PermissionDecision::AllowTool))
        .is_ok());

    let events = collect_all(stream).await;
    assert_eq!(describe(&events), vec!["token:allowed"]);
    assert!(pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_duplicate_provider_permission_id_is_refused() {
    let fixture = Fixture::new(
        vec![Round::Custom(Box::new(|services| {
            let interactions = services.interactions.expect("interaction executor");
            Box::pin(async_stream::stream! {
                let request = approval_request("perm-1", serde_json::json!({}));
                let first = interactions(request.clone());
                let second = interactions(request).await;
                drop(first);
                yield Ok(ChatStreamDelta::Text(decision_label(&second)));
            })
        }))],
        db_with_refreshes(0),
    );

    let events = fixture.run(&test_conversation(), Default::default()).await;

    let described = describe(&events);
    assert_eq!(described.len(), 1);
    assert!(
        described[0].contains("permission request 'perm-1' is already pending"),
        "{described:?}"
    );
    assert!(fixture.svc.pending_permissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sandbox_approval_is_registered_before_its_event_is_visible() {
    let mut fixture = Fixture::new(vec![], db_with_refreshes(5));
    let registry = fixture.svc.harness_mcp_entries.clone();
    fixture.svc.ai = Arc::new(TurnScript {
        rounds: Mutex::new(VecDeque::from([Round::Custom(Box::new(move |_| {
            let interactions = registry
                .lock()
                .unwrap()
                .values()
                .next()
                .expect("the turn registered its MCP bridge")
                .interactions
                .clone();
            Box::pin(async_stream::stream! {
                let decision = interactions(approval_request(
                    "perm-1",
                    serde_json::json!({ "api_key": "hunter2" }),
                ))
                .await;
                yield Ok(ChatStreamDelta::Text(decision_label(&decision)));
            })
        }))])),
        requests: fixture.requests.clone(),
        advance_per_call: None,
    });

    let mut stream = fixture.start(&harness_conversation(), harness()).await;

    let first = stream.next().await.expect("permission event").expect("ok");
    let ChatStreamEvent::PermissionRequested {
        id, kind, input, ..
    } = first
    else {
        panic!("expected a permission event, got {first:?}");
    };
    assert_eq!(id, "perm-1");
    assert_eq!(kind, PermissionKind::ToolApproval);
    assert!(!input.to_string().contains("hunter2"));
    let entry = take_pending(&fixture.svc, "perm-1");
    assert_eq!(entry.origin, PendingPermissionOrigin::Provider);
    assert!(entry
        .sender
        .send(resolution(PermissionDecision::AllowTool))
        .is_ok());

    let rest = collect_all(stream).await;
    assert_eq!(describe(&rest), vec!["token:allowed"]);
}

#[tokio::test]
async fn a_question_tool_shows_its_card_and_gives_the_answer_to_the_model() {
    let fixture = Fixture::new(
        vec![
            deltas(vec![ChatStreamDelta::ToolCall(tool_call(
                "q1",
                crate::ask_user::ASK_USER_TOOL,
                r#"{"questions":[{"question":"Which database?"}]}"#,
            ))]),
            deltas(vec![text("Thanks.")]),
        ],
        db_with_refreshes(1),
    );

    let mut stream = fixture
        .start(&test_conversation(), Default::default())
        .await;

    assert!(matches!(
        stream.next().await,
        Some(Ok(ChatStreamEvent::ToolCall { .. }))
    ));
    let card = stream.next().await.expect("card event").expect("ok");
    let ChatStreamEvent::PermissionRequested { id, kind, .. } = card else {
        panic!("expected a card, got {card:?}");
    };
    assert_eq!(kind, PermissionKind::Question);
    let entry = take_pending(&fixture.svc, &id);
    assert_eq!(entry.origin, PendingPermissionOrigin::PlatformWrite);
    assert!(entry
        .sender
        .send(resolution(PermissionDecision::AnswerQuestion {
            answers: serde_json::json!({ "Which database?": "PostgreSQL" }),
        }))
        .is_ok());

    let rest = collect_all(stream).await;
    assert_eq!(
        describe(&rest).last().map(String::as_str),
        Some("token:Thanks.")
    );
    let requests = fixture.requests();
    let answer = requests[1].messages.last().expect("tool answer");
    assert_eq!(answer.role, "tool");
    assert!(answer.content.contains("PostgreSQL"), "{}", answer.content);
}

// ---- Turn ownership and cancellation -------------------------------------

#[tokio::test]
async fn an_owned_turn_finishes_completed_with_turn_complete_last() {
    let conversation = harness_conversation();
    let db = db_with_refreshes(0).append_query_results([[claimed(conversation.clone(), "turn-1")]]);
    let fixture = Fixture::new(vec![deltas(vec![text("Hi")])], db);
    let mut live = fixture.svc.subscribe_conversation(conversation.id);

    let events = fixture
        .run(
            &conversation,
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(describe(&events), vec!["token:Hi"]);
    assert_eq!(
        wait_for_turn_complete(&mut live).await,
        vec!["token", "turn_complete"]
    );
    assert!(fixture.svc.active_turns.lock().unwrap().is_empty());
    let log = fixture.into_log().await;
    let last = writes_to(&log, CONVERSATION_UPDATE)
        .pop()
        .expect("terminal status");
    assert!(string_values(last).contains(&"completed".to_string()));
    assert!(string_values(last).contains(&"turn-1".to_string()));
}

#[tokio::test]
async fn a_failed_turn_finishes_failed_with_its_detail_on_the_conversation() {
    let conversation = harness_conversation();
    let claim = claimed(conversation.clone(), "turn-1");
    let db = db_with_refreshes(0)
        .append_query_results([[claim.clone()]])
        .append_query_results([[claim.clone()]])
        .append_query_results([[claim]]);
    let fixture = Fixture::new(vec![Round::OpenErr(provider_error("boom"))], db);
    let mut live = fixture.svc.subscribe_conversation(conversation.id);

    fixture
        .run(
            &conversation,
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(
        wait_for_turn_complete(&mut live).await,
        vec!["error", "turn_complete"]
    );
    let log = fixture.into_log().await;
    let updates = writes_to(&log, CONVERSATION_UPDATE);
    assert_eq!(updates.len(), 3, "safe detail, fallback detail, status");
    assert!(json_value(updates[0])["last_failure"]["detail"]
        .as_str()
        .is_some_and(|detail| detail != "The AI turn stopped before Temps retained a safe failure detail. Retry the message; if it fails again, open session diagnostics."));
    assert_eq!(
        json_value(updates[1])["last_failure"]["code"],
        "turn_failed_without_detail"
    );
    assert!(string_values(updates[2]).contains(&"failed".to_string()));
}

#[tokio::test]
async fn the_durable_permission_mode_applies_from_the_start_of_the_turn() {
    let conversation = harness_conversation();
    let mut claim = claimed(conversation.clone(), "turn-1");
    claim.ai_permission_mode = "auto".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results((0..8).map(|_| MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }))
        .append_query_results([[claim]])
        .append_query_results([[api_key_row()]])
        .append_query_results([[test_auth().user.expect("test user")]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]]);
    let fixture = Fixture::new(
        vec![awaits_interaction(approval_request(
            "perm-1",
            serde_json::json!({}),
        ))],
        db,
    );

    let events = fixture
        .run(
            &conversation,
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(describe(&events), vec!["token:allowed"]);
    assert!(fixture.svc.pending_permissions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_ownership_check_reports_it_and_never_reaches_the_provider() {
    let fixture = Fixture::new(
        vec![deltas(vec![text("must not run")])],
        db_with_refreshes(0),
    );

    let events = fixture
        .run(
            &test_conversation(),
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;

    assert_eq!(
        describe(&events),
        vec![
            "error:AI provider error: The server could not verify this turn before execution. \
             Please try again."
        ]
    );
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn stopping_a_turn_aborts_the_provider_and_persists_no_message() {
    let conversation = harness_conversation();
    let claim = claimed(conversation.clone(), "turn-1");
    let db = db_with_refreshes(0)
        .append_query_results([[claim.clone()]])
        .append_query_results([[claim.clone()]]);
    let fixture = Fixture::new(
        vec![Round::Custom(Box::new(|_| {
            Box::pin(async_stream::stream! {
                yield Ok(ChatStreamDelta::Text("Working".to_string()));
                futures::future::pending::<()>().await;
            })
        }))],
        db,
    );
    let mut live = fixture.svc.subscribe_conversation(conversation.id);

    let mut stream = fixture
        .start(
            &conversation,
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(
        stream.next().await,
        Some(Ok(ChatStreamEvent::Token(_)))
    ));
    assert_eq!(live.recv().await.expect("live token").event, "token");

    assert!(fixture.svc.cancel_turn(&claim).await.expect("cancel"));

    let rest = tokio::time::timeout(Duration::from_secs(5), collect_all(stream))
        .await
        .expect("the viewer stream must end once the turn is stopped");
    assert!(rest.is_empty(), "{rest:?}");
    // One from `cancel_turn` and one from the monitor.
    assert_eq!(
        wait_for_turn_complete(&mut live).await,
        vec!["turn_complete"]
    );
    assert_eq!(
        wait_for_turn_complete(&mut live).await,
        vec!["turn_complete"]
    );
    assert!(fixture.svc.active_turns.lock().unwrap().is_empty());

    let log = fixture.into_log().await;
    assert!(writes_to(&log, MESSAGE_INSERT).is_empty());
    let updates = writes_to(&log, CONVERSATION_UPDATE);
    assert!(string_values(updates[0]).contains(&"cancelled".to_string()));
    assert!(string_values(updates.last().expect("monitor status")).contains(&"failed".to_string()));
}

#[tokio::test]
async fn stopping_a_turn_withdraws_its_pending_approval() {
    let conversation = harness_conversation();
    let claim = claimed(conversation.clone(), "turn-1");
    let db = db_with_refreshes(0)
        .append_query_results([[claim.clone()]])
        .append_query_results([[claim.clone()]]);
    let fixture = Fixture::new(
        vec![awaits_interaction(approval_request(
            "perm-1",
            serde_json::json!({}),
        ))],
        db,
    );

    let stream = fixture
        .start(
            &conversation,
            TurnOptions {
                turn_id: Some("turn-1".to_string()),
                ..Default::default()
            },
        )
        .await;
    let pending = fixture.svc.pending_permissions.clone();
    eventually("the approval to be registered", || {
        pending.lock().unwrap().contains_key("perm-1")
    })
    .await;

    assert!(fixture.svc.cancel_turn(&claim).await.expect("cancel"));

    let rest = tokio::time::timeout(Duration::from_secs(5), collect_all(stream))
        .await
        .expect("the viewer stream must end once the turn is stopped");
    assert!(rest.is_empty(), "{rest:?}");
    eventually("the approval to be withdrawn", || {
        pending.lock().unwrap().is_empty()
    })
    .await;
}

// ---- Stage: interactions ---------------------------------------------------

type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<Result<ChatStreamEvent, ChatError>>;

fn stage_events() -> (TurnEvents, EventReceiver) {
    #[allow(
        clippy::disallowed_methods,
        reason = "test: mirrors the turn's own event queue"
    )]
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (live, _) = broadcast::channel(16);
    (TurnEvents { tx, live }, rx)
}

fn stage_authorization(db: MockDatabase, uses_harness: bool) -> TurnAuthorization {
    TurnAuthorization {
        db: Arc::new(db.into_connection()),
        auth: api_key_auth(),
        project_access_checker: None,
        application_id: None,
        harness_project_ids: Vec::new(),
        provider: "claude_cli".to_string(),
        permission_mode: Arc::new(Mutex::new("default".to_string())),
        uses_harness,
    }
}

type PendingRegistry = Arc<Mutex<HashMap<String, PendingPermissionEntry>>>;

fn stage_registrar() -> (PermissionRegistrar, PendingRegistry) {
    let registry = Arc::new(Mutex::new(HashMap::new()));
    (
        permission_registrar("pub1".to_string(), registry.clone()),
        registry,
    )
}

/// A registrar during which the permission mode switches to Auto.
fn registrar_that_enables_auto(
    registrar: PermissionRegistrar,
    auto_approve: Arc<AtomicBool>,
) -> PermissionRegistrar {
    Arc::new(move |request, origin| {
        let pending = registrar(request, origin);
        auto_approve.store(true, Ordering::Release);
        pending
    })
}

fn platform_stage(
    db: MockDatabase,
    registrar: PermissionRegistrar,
    auto_approve: bool,
    autofix_incident: Option<&str>,
) -> (PlatformInteractionExecutor, EventReceiver) {
    let (events, rx) = stage_events();
    let executor = platform_interactions(PlatformInteractionContext {
        registrar,
        auto_approve: Arc::new(AtomicBool::new(auto_approve)),
        events,
        auth: api_key_auth(),
        metadata: test_request_metadata(),
        db: Arc::new(db.into_connection()),
        autofix_incident: autofix_incident.map(str::to_string),
    });
    (executor, rx)
}

fn lapsed(error: AiError, purpose: &str) {
    match error {
        AiError::Provider {
            purpose: actual,
            reason,
        } => {
            assert_eq!(actual, purpose);
            assert_eq!(reason, "the initiating credential is no longer authorized");
        }
        other => panic!("expected a lapsed credential, got {other}"),
    }
}

#[tokio::test]
async fn an_abandoned_permission_future_removes_its_registration() {
    let (registrar, registry) = stage_registrar();
    let pending = registrar(
        approval_request("perm-1", serde_json::json!({})),
        PendingPermissionOrigin::Provider,
    );
    // Registered by the call itself, before the future is ever polled.
    assert!(registry.lock().unwrap().contains_key("perm-1"));
    // Dropped without ever being polled, as a stopped turn drops it.
    drop(pending);
    assert!(registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_resolver_that_goes_away_ends_the_wait_with_a_reason() {
    let (registrar, registry) = stage_registrar();
    let pending = registrar(
        approval_request("perm-1", serde_json::json!({})),
        PendingPermissionOrigin::Provider,
    );
    registry.lock().unwrap().remove("perm-1");

    let error = pending.await.err().expect("no resolver left");
    assert!(
        error
            .to_string()
            .contains("permission request 'perm-1' ended before it was resolved"),
        "{error}"
    );
}

#[tokio::test]
async fn provider_auto_approval_rechecks_the_credential() {
    let (registrar, registry) = stage_registrar();
    let interactions = provider_interactions(
        registrar,
        Arc::new(AtomicBool::new(true)),
        stage_authorization(db_with_refreshes(1), false),
    );

    let decision = interactions(approval_request("perm-1", serde_json::json!({}))).await;

    assert!(matches!(decision, Ok(PermissionDecision::AllowTool)));
    assert!(registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_auto_approval_is_denied_once_the_credential_lapses() {
    let (registrar, registry) = stage_registrar();
    let interactions = provider_interactions(
        registrar,
        Arc::new(AtomicBool::new(true)),
        stage_authorization(revoke_key(db_with_refreshes(0)), false),
    );

    let error = interactions(approval_request("perm-1", serde_json::json!({})))
        .await
        .err()
        .expect("denied");

    lapsed(error, "chat.permission.authorization");
    assert!(registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_auto_approval_in_a_harness_checks_its_runtime_too() {
    // The refresh succeeds as a credential, but a chat in a harness has to pass
    // the harness runtime's checks as well: this provider is not a harness.
    let (registrar, _registry) = stage_registrar();
    let mut authorization = stage_authorization(db_with_refreshes(1), true);
    authorization.provider = "not-a-harness".to_string();
    let interactions =
        provider_interactions(registrar, Arc::new(AtomicBool::new(true)), authorization);

    let error = interactions(approval_request("perm-1", serde_json::json!({})))
        .await
        .err()
        .expect("denied");

    lapsed(error, "chat.permission.authorization");
}

#[tokio::test]
async fn a_mode_switch_during_registration_approves_without_stranding_the_request() {
    let (registrar, registry) = stage_registrar();
    let auto_approve = Arc::new(AtomicBool::new(false));
    let interactions = provider_interactions(
        registrar_that_enables_auto(registrar, auto_approve.clone()),
        auto_approve,
        stage_authorization(db_with_refreshes(1), false),
    );

    let decision = interactions(approval_request("perm-1", serde_json::json!({}))).await;

    assert!(matches!(decision, Ok(PermissionDecision::AllowTool)));
    assert!(registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_questions_are_never_auto_approved() {
    let (registrar, registry) = stage_registrar();
    let interactions = provider_interactions(
        registrar,
        Arc::new(AtomicBool::new(true)),
        stage_authorization(db_with_refreshes(0), false),
    );
    let mut request = approval_request("q-1", serde_json::json!({}));
    request.kind = PermissionKind::Question;

    let pending = interactions(request);

    let entry = registry.lock().unwrap().remove("q-1").expect("registered");
    assert_eq!(entry.origin, PendingPermissionOrigin::Provider);
    assert!(entry
        .sender
        .send(resolution(PermissionDecision::AllowTool))
        .is_ok());
    assert!(pending.await.is_ok());
}

#[tokio::test]
async fn sandbox_auto_approval_shows_no_card() {
    let (registrar, registry) = stage_registrar();
    let auto_approve = Arc::new(AtomicBool::new(true));
    let provider = provider_interactions(
        registrar,
        auto_approve.clone(),
        stage_authorization(db_with_refreshes(1), false),
    );
    let (events, mut rx) = stage_events();
    let sandbox = sandbox_interactions(provider, auto_approve, events);

    let decision = sandbox(approval_request("perm-1", serde_json::json!({}))).await;

    assert!(matches!(decision, Ok(PermissionDecision::AllowTool)));
    assert!(registry.lock().unwrap().is_empty());
    assert!(
        rx.try_recv().is_err(),
        "an approved request is no ghost card"
    );
}

#[tokio::test]
async fn an_autofix_chat_refuses_platform_writes_without_asking() {
    let (registrar, registry) = stage_registrar();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::autofix_incidents::Model>::new()]);
    let (executor, mut rx) = platform_stage(db, registrar, false, Some("incident-1"));

    let resolved = executor(approval_request("perm-1", serde_json::json!({})))
        .await
        .expect("a refusal, not an error");

    assert!(matches!(
        resolved.decision,
        PermissionDecision::DenyTool { reason: Some(reason) }
            if reason == crate::autofix::REFUSED_WRITE_REASON
    ));
    assert!(registry.lock().unwrap().is_empty());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn platform_auto_approval_uses_the_freshly_refreshed_credential() {
    let (registrar, registry) = stage_registrar();
    let (executor, mut rx) = platform_stage(db_with_refreshes(1), registrar, true, None);

    let resolved = executor(approval_request("perm-1", serde_json::json!({})))
        .await
        .expect("approved");

    assert!(matches!(resolved.decision, PermissionDecision::AllowTool));
    assert!(registry.lock().unwrap().is_empty());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn platform_auto_approval_is_denied_once_the_credential_lapses() {
    let (registrar, _registry) = stage_registrar();
    let (executor, _rx) = platform_stage(revoke_key(db_with_refreshes(0)), registrar, true, None);

    let error = executor(approval_request("perm-1", serde_json::json!({})))
        .await
        .err()
        .expect("denied");

    lapsed(error, "chat.permission.authorization");
}

#[tokio::test]
async fn a_destructive_platform_write_still_asks_in_auto_mode() {
    let (registrar, registry) = stage_registrar();
    let (executor, mut rx) = platform_stage(db_with_refreshes(0), registrar, true, None);

    let pending = executor(approval_request(
        "perm-1",
        serde_json::json!({ "method": "DELETE", "api_key": "hunter2" }),
    ));

    let event = rx.try_recv().expect("the card").expect("ok");
    let ChatStreamEvent::PermissionRequested { id, input, .. } = event else {
        panic!("expected a card, got {event:?}");
    };
    assert_eq!(id, "perm-1");
    assert!(!input.to_string().contains("hunter2"));
    let entry = registry
        .lock()
        .unwrap()
        .remove("perm-1")
        .expect("registered");
    assert_eq!(entry.origin, PendingPermissionOrigin::PlatformWrite);
    assert!(entry
        .sender
        .send(resolution(PermissionDecision::AllowTool))
        .is_ok());
    assert!(pending.await.is_ok());
}

#[tokio::test]
async fn a_platform_mode_switch_during_registration_shows_no_card() {
    let (registrar, _registry) = stage_registrar();
    let auto_approve = Arc::new(AtomicBool::new(false));
    let (events, mut rx) = stage_events();
    let executor = platform_interactions(PlatformInteractionContext {
        registrar: registrar_that_enables_auto(registrar, auto_approve.clone()),
        auto_approve,
        events,
        auth: api_key_auth(),
        metadata: test_request_metadata(),
        db: Arc::new(db_with_refreshes(1).into_connection()),
        autofix_incident: None,
    });

    let resolved = executor(approval_request("perm-1", serde_json::json!({})))
        .await
        .expect("approved");

    assert!(matches!(resolved.decision, PermissionDecision::AllowTool));
    assert!(rx.try_recv().is_err());
}

fn stage_router(provider: Arc<dyn ConversationContextProvider>) -> ToolRouter {
    let (registrar, _registry) = stage_registrar();
    let (interactions, _rx) = platform_stage(db_with_refreshes(0), registrar, false, None);
    ToolRouter {
        project_id: Some(7),
        conversation_id: 1,
        context_id: "42".to_string(),
        project_access_checker: None,
        provider: Some(provider),
        api_tools: None,
        repo_tools: None,
        write_handle: None,
        pending: None,
        audit: None,
        interactions,
        state: Arc::new(tokio::sync::Mutex::new(ToolExecutionState::default())),
    }
}

#[tokio::test]
async fn the_tool_executor_refreshes_the_credential_before_each_call() {
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let router = stage_router(Arc::new(StubProvider {
        tool_calls: tool_calls.clone(),
    }));
    let executor = turn_tool_executor(
        router,
        stage_authorization(revoke_key(db_with_refreshes(1)), false),
    );

    let result = executor(tool_call("c1", "echo", "{}")).await.expect("ran");
    assert_eq!(result, "tool result");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);

    let error = executor(tool_call("c2", "echo", "{\"n\":2}"))
        .await
        .err()
        .expect("denied");
    lapsed(error, "chat.tool.authorization");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_tool_router_shares_its_repeat_guard_across_paths() {
    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let router = stage_router(Arc::new(StubProvider {
        tool_calls: tool_calls.clone(),
    }));
    let call = tool_call("c1", "echo", "{}");

    assert_eq!(
        router.clone().dispatch(&call, &test_auth()).await,
        "tool result"
    );
    let repeat = router.dispatch(&call, &test_auth()).await;

    assert!(repeat.starts_with("You already ran `echo`"), "{repeat}");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
}

const PROCESS_START_ARGUMENTS: &str = r#"{"name":"web","program":"node"}"#;

fn process_executor(
    authorization: TurnAuthorization,
    decision: Option<PermissionDecision>,
    asked: Arc<std::sync::atomic::AtomicUsize>,
) -> ManagedProcessExecutor {
    let interactions: temps_ai::InteractionExecutor = Arc::new(move |_| {
        asked.fetch_add(1, Ordering::SeqCst);
        let decision = decision.clone();
        Box::pin(async move { decision.ok_or(AiError::NotAvailable) })
    });
    managed_process_executor(
        Arc::new(TurnScript {
            rounds: Mutex::new(VecDeque::new()),
            requests: Arc::new(Mutex::new(Vec::new())),
            advance_per_call: None,
        }),
        temps_ai::HarnessWorkspace {
            sandbox_label: "app_tool_loop".to_string(),
            host_work_dir: std::path::PathBuf::from("/tmp/app_tool_loop"),
        },
        authorization,
        interactions,
        Vec::new(),
    )
}

#[tokio::test]
async fn a_managed_process_start_needs_approval() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor = process_executor(
        stage_authorization(db_with_refreshes(1), true),
        Some(PermissionDecision::DenyTool { reason: None }),
        asked.clone(),
    );

    let error = executor(tool_call("p1", PROCESS_START_TOOL, PROCESS_START_ARGUMENTS))
        .await
        .err()
        .expect("denied");

    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(
        error
            .to_string()
            .contains("the managed process operation was not approved"),
        "{error}"
    );
}

#[tokio::test]
async fn a_managed_process_status_does_not_ask() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor = process_executor(
        stage_authorization(db_with_refreshes(1), true),
        None,
        asked.clone(),
    );

    // Reaches the runtime, which this script does not provide.
    let error = executor(tool_call(
        "p1",
        PROCESS_STATUS_TOOL,
        r#"{"process_id":"proc_1"}"#,
    ))
    .await
    .err()
    .expect("no runtime");

    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(!error.to_string().contains("not approved"), "{error}");
    assert!(
        !error.to_string().contains("no longer authorized"),
        "{error}"
    );
}

#[tokio::test]
async fn an_approved_managed_process_is_rechecked_before_it_runs() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor = process_executor(
        stage_authorization(revoke_key(db_with_refreshes(1)), true),
        Some(PermissionDecision::AllowTool),
        asked.clone(),
    );

    let error = executor(tool_call("p1", PROCESS_START_TOOL, PROCESS_START_ARGUMENTS))
        .await
        .err()
        .expect("denied");

    assert_eq!(asked.load(Ordering::SeqCst), 1);
    lapsed(error, "chat.runtime_process.authorization");
}

#[tokio::test]
async fn invalid_managed_process_input_is_refused_before_any_check() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor = process_executor(
        stage_authorization(MockDatabase::new(DatabaseBackend::Postgres), true),
        None,
        asked.clone(),
    );

    let error = executor(tool_call("p1", PROCESS_START_TOOL, "not json"))
        .await
        .err()
        .expect("refused");

    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(
        error
            .to_string()
            .contains("the managed process start input is invalid"),
        "{error}"
    );
}
