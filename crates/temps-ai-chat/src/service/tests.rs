// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

macro_rules! mcp_request {
    ($($json:tt)*) => {
        serde_json::from_value::<temps_ai::mcp::McpRequest>(serde_json::json!($($json)*))
            .expect("valid MCP request fixture")
    };
}
use super::*;

#[test]
fn durable_failure_metadata_is_safe_preserved_and_clearable() {
    let existing = serde_json::json!({ "application_id": 42 });
    let failure =
        ChatError::Ai("token=super-secret OAuth credentials are not supported".to_string())
            .public_failure();
    let stored = conversation_metadata_with_failure(Some(&existing), Some(&failure))
        .expect("metadata should remain present");

    assert_eq!(stored["application_id"], 42);
    assert_eq!(
        stored["last_failure"]["code"],
        "unsupported_workspace_credential"
    );
    assert!(!stored.to_string().contains("super-secret"));

    let cleared = conversation_metadata_with_failure(Some(&stored), None)
        .expect("unrelated metadata should remain");
    assert_eq!(cleared, existing);
}

#[test]
fn context_usage_is_latest_snapshot_not_a_cumulative_counter() {
    let first = PersistedContextWindowUsage {
        used_tokens: 10_000,
        limit_tokens: None,
        model: Some("model-a".to_string()),
        source: "provider_reported".to_string(),
        estimated: true,
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let second = PersistedContextWindowUsage {
        used_tokens: 4_000,
        updated_at: "2026-01-01T00:01:00Z".to_string(),
        ..first.clone()
    };
    let existing = serde_json::json!({ "last_failure": { "code": "kept" } });
    let after_first =
        conversation_metadata_with_context_usage(Some(&existing), &first).expect("usage metadata");
    let after_second = conversation_metadata_with_context_usage(Some(&after_first), &second)
        .expect("replacement usage metadata");

    assert_eq!(after_second["context_usage"]["used_tokens"], 4_000);
    assert_eq!(after_second["last_failure"]["code"], "kept");
    let reset = conversation_metadata_without_context_usage(Some(&after_second))
        .expect("unrelated metadata remains");
    assert!(reset.get("context_usage").is_none());
    assert_eq!(reset["last_failure"]["code"], "kept");
}

#[test]
fn context_usage_wire_event_has_stable_provider_reported_shape() {
    let event = wire_event_for(&Ok(ChatStreamEvent::ContextUsage(
        PersistedContextWindowUsage {
            used_tokens: 42,
            limit_tokens: None,
            model: Some("model-a".to_string()),
            source: "provider_reported".to_string(),
            estimated: true,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        },
    )));
    let payload: serde_json::Value = serde_json::from_str(&event.data).expect("wire usage JSON");

    assert_eq!(event.event, "context_usage");
    assert_eq!(payload["used_tokens"], 42);
    assert!(payload["limit_tokens"].is_null());
    assert_eq!(payload["estimated"], true);
}

#[tokio::test]
async fn durable_failure_update_is_scoped_to_the_owning_turn() {
    let mut conversation = test_conversation();
    conversation.active_turn_id = Some("turn-newer".to_string());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[conversation]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    let failure = ChatError::Ai("provider returned no response".to_string()).public_failure();

    persist_conversation_failure(&db, 42, Some("turn-older"), &failure)
        .await
        .expect("a stale writer should fail closed without a database error");

    let statements = db
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect::<Vec<_>>();
    let update = statements
        .iter()
        .find(|statement| statement.sql.contains("UPDATE \"ai_conversations\""))
        .expect("failure persistence should issue an update");
    assert!(update.sql.contains("active_turn_id"));
    assert!(format!("{update:?}").contains("turn-older"));
}

#[tokio::test]
async fn missing_session_recovery_is_scoped_to_the_owning_turn() {
    let mut conversation = test_conversation();
    conversation.active_turn_id = Some("turn-newer".to_string());
    conversation.metadata = Some(serde_json::json!({
        "context_usage": {
            "used_tokens": 10,
            "source": "provider_reported"
        }
    }));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[conversation]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();

    clear_missing_provider_session(&db, 42, Some("turn-older"))
        .await
        .expect("a stale recovery should fail closed without a database error");

    let statements = db
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect::<Vec<_>>();
    let update = statements
        .iter()
        .find(|statement| statement.sql.contains("UPDATE \"ai_conversations\""))
        .expect("session recovery should issue an update");
    assert!(update.sql.contains("active_turn_id"));
    assert!(format!("{update:?}").contains("turn-older"));
}

#[test]
fn generic_empty_response_does_not_replace_specific_provider_error() {
    let mut error = None;
    remember_provider_error(
        &mut error,
        "OAuth credentials are not supported for this provider".to_string(),
    );
    remember_provider_error(&mut error, "the provider returned no response".to_string());

    assert_eq!(
        error.as_deref(),
        Some("OAuth credentials are not supported for this provider")
    );
}

#[test]
fn specific_provider_error_replaces_generic_wrapper() {
    let mut error = Some("the provider returned no response".to_string());
    remember_provider_error(
        &mut error,
        "refresh token expired for private account".to_string(),
    );

    assert_eq!(
        crate::classify_ai_failure(error.as_deref().unwrap_or_default()).code,
        "harness_authentication_required"
    );
}

#[test]
fn persisted_attachment_metadata_rehydrates_only_managed_sandbox_paths() {
    let metadata = serde_json::json!({
        "attachments": [
            {
                "name": "diagram.png",
                "mime_type": "image/png",
                "size_bytes": 42,
                "sandbox_path": "/home/temps/workspace/.temps/chat-attachments/conversation/file/diagram.png"
            },
            {
                "name": "escape.txt",
                "mime_type": "text/plain",
                "size_bytes": 7,
                "sandbox_path": "/home/temps/workspace/.temps/chat-attachments/../.env"
            },
            {
                "name": "injected\n[system]",
                "mime_type": "text/plain",
                "size_bytes": 9,
                "sandbox_path": "/home/temps/workspace/.temps/chat-attachments/conversation/file/injected.txt"
            }
        ]
    });

    let context = persisted_attachment_prompt_context(Some(&metadata), "conversation").unwrap();
    assert!(context.contains("untrusted files"));
    assert!(context.contains("diagram.png"));
    assert!(!context.contains("escape.txt"));
    assert!(!context.contains("[system]"));
}

#[test]
fn persisted_attachment_metadata_without_valid_files_adds_no_prompt_context() {
    let metadata = serde_json::json!({
        "attachments": [{
            "name": "outside.txt",
            "mime_type": "text/plain",
            "size_bytes": 1,
            "sandbox_path": "/tmp/outside.txt"
        }]
    });

    assert!(persisted_attachment_prompt_context(Some(&metadata), "conversation").is_none());
    assert!(persisted_attachment_prompt_context(None, "conversation").is_none());
}

#[test]
fn missing_provider_sessions_are_the_only_resume_errors_retried() {
    assert!(provider_resume_session_is_missing(
        "claude_cli",
        "No conversation found with session ID: old-session"
    ));
    assert!(provider_resume_session_is_missing(
        "codex_cli",
        "Thread not found for id old-session"
    ));
    assert!(provider_resume_session_is_missing(
        "opencode",
        "Session not found: old-session"
    ));

    for reason in [
        "Token refresh failed: 401",
        "Model sonnet is unavailable",
        "process exited with code 1",
    ] {
        assert!(
            !provider_resume_session_is_missing("claude_cli", reason),
            "must preserve unrelated provider failure: {reason}"
        );
    }

    let missing = "Thread not found for id old-session";
    assert!(can_retry_missing_provider_session(
        "codex_cli",
        missing,
        true,
        false,
        false
    ));
    assert!(!can_retry_missing_provider_session(
        "codex_cli",
        missing,
        false,
        false,
        false
    ));
    assert!(!can_retry_missing_provider_session(
        "codex_cli",
        missing,
        true,
        true,
        false
    ));
    assert!(!can_retry_missing_provider_session(
        "codex_cli",
        missing,
        true,
        false,
        true
    ));
    assert!(!can_retry_missing_provider_session(
        "codex_cli",
        "Token refresh failed: 401",
        true,
        false,
        false
    ));
}

#[tokio::test]
async fn startup_missing_session_reacquires_once_before_answering() {
    let ai = Arc::new(ScriptedAi::new(vec![
        Err(AiError::Provider {
            purpose: "chat.application.tools".to_string(),
            reason: "Thread not found for id old-session".to_string(),
        }),
        Ok(vec![
            ChatStreamDelta::SessionMetadata {
                session_id: Some("new-session".to_string()),
                title: None,
            },
            ChatStreamDelta::Text("Recovered answer".to_string()),
        ]),
    ]));
    let requests = ai.requests.clone();
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "codex_cli".to_string();
    conversation.ai_permission_mode = "auto".to_string();
    conversation.cli_session_id = Some("old-session".to_string());
    let (service, _tools, auth) =
        service_with_current_tool_auth_and_session_clear(ai, Some(conversation.clone()));
    let stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &auth,
            &test_request_metadata(),
            None,
            vec![],
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_missing_session".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_missing_session"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            vec![],
            None,
            false,
            None,
        )
        .await;
    let mut stream = stream;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => output.push(event),
            Err(error) => errors.push(error.to_string()),
        }
    }
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "missing session gets one retry only; output={output:?}, errors={errors:?}"
    );
    assert!(
        output.contains(&ChatStreamEvent::Token("Recovered answer".to_string())),
        "{output:?}"
    );
    assert_eq!(
        requests[0].resume_session_id.as_deref(),
        Some("old-session")
    );
    assert!(!requests[0].reset_retained_session);
    assert_eq!(requests[1].resume_session_id, None);
    assert!(requests[1].reset_retained_session);
    assert_eq!(requests[0].trace_id, requests[1].trace_id);
}

#[tokio::test]
async fn streamed_missing_session_discards_stale_metadata_before_retry() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let ai = Arc::new(MissingSessionStreamAi {
        requests: requests.clone(),
    });
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "codex_cli".to_string();
    conversation.ai_permission_mode = "auto".to_string();
    conversation.cli_session_id = Some("old-session".to_string());
    let (service, _tools, auth) =
        service_with_current_tool_auth_and_session_clear(ai, Some(conversation.clone()));
    let stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &auth,
            &test_request_metadata(),
            None,
            vec![],
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_stream_missing_session".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_stream_missing_session"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            vec![],
            None,
            false,
            None,
        )
        .await;
    let output = drain(stream).await;
    assert!(
        output.contains(&ChatStreamEvent::Token("Stream recovered".to_string())),
        "{output:?}"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].resume_session_id.as_deref(),
        Some("old-session")
    );
    assert!(!requests[0].reset_retained_session);
    assert_eq!(requests[1].resume_session_id, None);
    assert!(requests[1].reset_retained_session);
}

#[tokio::test]
async fn absent_durable_session_resets_stale_retained_runtime_on_first_round() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
        "Fresh session".to_string(),
    )])]));
    let requests = ai.requests.clone();
    let (service, _tools, auth) = service_with_current_tool_auth(ai);
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "codex_cli".to_string();
    conversation.ai_permission_mode = "auto".to_string();
    conversation.cli_session_id = None;
    let stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &auth,
            &test_request_metadata(),
            None,
            vec![],
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_absent_session".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_absent_session"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            vec![],
            None,
            false,
            None,
        )
        .await;
    let output = drain(stream).await;
    assert!(output.contains(&ChatStreamEvent::Token("Fresh session".to_string())));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].resume_session_id, None);
    assert!(requests[0].reset_retained_session);
}

#[test]
fn changing_models_starts_a_fresh_cli_session() {
    assert_eq!(
        cli_session_after_model_change("sonnet", "sonnet", Some("session-1")).as_deref(),
        Some("session-1")
    );
    assert_eq!(
        cli_session_after_model_change("opus[1m]", "sonnet", Some("session-1")),
        None
    );
    assert_eq!(
        cli_session_fingerprint_after_model_change("sonnet", "sonnet", Some("v1:fingerprint"))
            .as_deref(),
        Some("v1:fingerprint")
    );
    assert_eq!(
        cli_session_fingerprint_after_model_change("opus[1m]", "sonnet", Some("v1:fingerprint")),
        None
    );
}

use std::collections::HashMap;

#[test]
fn productive_results_are_distinguished_from_rejections() {
    // The read CLI's success shape — a contract, not a guess.
    assert!(tool_result_is_productive(
        r#"{"operation":"query_metrics","status":200,"data":{"data":[]}}"#
    ));
    // A non-2xx replayed through the CLI is not progress.
    assert!(!tool_result_is_productive(
        r#"{"operation":"get_alert","status":404,"data":null}"#
    ));
    // The write tool's proposal receipt has no status and is very much progress.
    assert!(tool_result_is_productive(
        r#"{"status":"proposed","action_id":"abc","operation":"create_alert"}"#
    ));

    for rejection in [
        "Unknown parameter(s) for operation 'query_metrics': 'metric'.",
        "Missing required parameters for operation 'create_alert': name.",
        "Parameter 'detection_config' has the wrong shape for operation 'create_alert'.",
        "`query_metrics` failed: something went wrong",
        "You already ran `echo` with these exact arguments earlier this turn.",
        "Not staged: `create_alert` requires a backtest first.",
    ] {
        assert!(
            !tool_result_is_productive(rejection),
            "should count as unproductive: {rejection}"
        );
    }

    // An unrecognised tool's prose must NOT be mistaken for a failure —
    // misclassifying it would cut a working turn short.
    assert!(tool_result_is_productive(
        "The deployment finished at 12:04 and the container is healthy."
    ));
}

fn inline_action(status: &str, error: Option<&str>) -> temps_entities::ai_pending_actions::Model {
    temps_entities::ai_pending_actions::Model {
        id: 1,
        public_id: "action-1".to_string(),
        conversation_id: 1,
        message_id: None,
        project_id: Some(7),
        plan_public_id: None,
        step_index: 0,
        operation_id: "create_service".to_string(),
        method: "POST".to_string(),
        summary: "Create service".to_string(),
        params: serde_json::json!({}),
        required_permission: Some("projects:write".to_string()),
        status: status.to_string(),
        result: (status == "executed").then(|| serde_json::json!({"id": 42})),
        error: error.map(str::to_string),
        created_by: 1,
        confirmed_by: Some(1),
        created_at: Utc::now(),
        confirmed_at: Some(Utc::now()),
        executed_at: (status == "executed").then(Utc::now),
    }
}

#[test]
fn inline_write_failure_becomes_model_visible_tool_evidence() {
    let result = pending_action_tool_result(&inline_action(
        "failed",
        Some("upstream rejected the requested database version"),
    ));
    let value: serde_json::Value = serde_json::from_str(&result).expect("valid tool JSON");
    assert_eq!(value["status"], "failed");
    assert_eq!(value["operation"], "create_service");
    assert!(value["error"]
        .as_str()
        .is_some_and(|error| error.contains("database version")));
    assert!(value["instruction"]
        .as_str()
        .is_some_and(|instruction| instruction.contains("Diagnose")));
}

#[test]
fn inline_write_success_is_not_reported_as_a_detached_proposal() {
    let result = pending_action_tool_result(&inline_action("executed", None));
    let value: serde_json::Value = serde_json::from_str(&result).expect("valid tool JSON");
    assert_eq!(value["status"], "executed");
    assert_eq!(value["result"]["id"], 42);
    assert_ne!(value["status"], "proposed");
}

/// What is retained for recall must be bounded, or a turn holds every tool
/// result at full size in memory while the transcript trimming is busy
/// bounding the very same data.
#[test]
fn recall_copies_are_capped() {
    let small = "a short result";
    assert_eq!(summarize_for_recall(small), small);

    let huge = "x".repeat(200_000);
    let kept = summarize_for_recall(&huge);
    assert!(kept.len() < 5 * 1024, "not capped: {} bytes", kept.len());
    assert!(kept.contains("truncated for recall"));
}

/// Tool output is arbitrary text, so the cap has to land on a character
/// boundary — slicing blind panics on a multi-byte character across it.
#[test]
fn recall_cap_does_not_split_a_multibyte_character() {
    // 'é' is two bytes; a run of them guarantees a boundary at the cut.
    let multibyte = "é".repeat(100_000);
    let kept = summarize_for_recall(&multibyte);
    assert!(kept.contains("truncated for recall"));
}

/// The gate must read the operation in command position, not anywhere in
/// the string — an alert *named* `create_alert` must not make an unrelated
/// call demand a backtest.
#[test]
fn backtest_gate_ignores_operation_names_inside_arguments() {
    let seen = HashMap::new();
    assert!(
        missing_backtest(
            "deployments trigger_project_pipeline --name create_alert",
            &seen
        )
        .is_none(),
        "an operation name in an argument must not trip the gate"
    );
    // ...while the real thing still does, with or without a section prefix.
    assert!(missing_backtest("alerts create_alert --metric_name x", &seen).is_some());
    assert!(missing_backtest("create_alert --metric_name x", &seen).is_some());
}

fn assistant_msg(text: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: text.to_string(),
        ..Default::default()
    }
}

fn carried_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.len())
        .sum()
}

#[test]
fn carried_tool_results_are_trimmed_oldest_first() {
    let big = "x".repeat(1000);
    let mut messages = vec![
        ChatMessage::system("seed"),
        assistant_msg("round 1"),
        ChatMessage::tool("a".to_string(), big.clone()),
        assistant_msg("round 2"),
        ChatMessage::tool("b".to_string(), big.clone()),
        assistant_msg("round 3"),
        ChatMessage::tool("c".to_string(), big.clone()),
    ];

    trim_carried_tool_results(&mut messages, 1500);

    // Structure is preserved — a provider rejects a tool_calls block whose
    // answers have vanished, so messages are stubbed in place, never removed.
    assert_eq!(messages.len(), 7);
    assert_eq!(messages[2].tool_call_id.as_deref(), Some("a"));

    // Oldest went first; the newest result — what the model is actually
    // reasoning about — survives intact.
    assert!(messages[2].content.contains("dropped"));
    assert!(messages[4].content.contains("dropped"));
    assert_eq!(messages[6].content, big);
    assert!(carried_bytes(&messages) <= 1500);
}

#[test]
fn trimming_goes_below_the_budget_so_the_next_rounds_keep_a_stable_prefix() {
    let big = "x".repeat(1000);
    let mut messages = vec![ChatMessage::system("seed")];
    for id in ["a", "b", "c", "d", "e", "f"] {
        messages.push(assistant_msg("call"));
        messages.push(ChatMessage::tool(id.to_string(), big.clone()));
    }

    // Over a 5000-byte budget by one result: trims down to ~2500, not 5000.
    trim_carried_tool_results(&mut messages, 5000);
    let after_first = carried_bytes(&messages);
    assert!(after_first <= 2500 + 1000, "left {after_first} bytes");

    // The next round's result fits without touching anything older.
    let before = messages.clone();
    messages.push(assistant_msg("call"));
    messages.push(ChatMessage::tool("g".to_string(), big.clone()));
    trim_carried_tool_results(&mut messages, 5000);
    for (now, then) in messages.iter().zip(&before) {
        assert_eq!(now.content, then.content);
    }
}

#[test]
fn focus_project_is_read_from_conversation_metadata() {
    assert_eq!(
        focus_project_id(Some(&serde_json::json!({ "focus_project_id": 42 }))),
        Some(42)
    );
    assert_eq!(focus_project_id(None), None);
    assert_eq!(
        focus_project_id(Some(&serde_json::json!({ "scope": "user" }))),
        None
    );
    // A value that is not an id never names a project.
    assert_eq!(
        focus_project_id(Some(&serde_json::json!({ "focus_project_id": "7" }))),
        None
    );
    assert_eq!(
        focus_project_id(Some(
            &serde_json::json!({ "focus_project_id": 99999999999i64 })
        )),
        None
    );
}

#[test]
fn focus_project_section_names_the_project_and_keeps_it_a_default() {
    let section = focus_project_section("Storefront", "storefront", 12);
    assert!(section.starts_with("## Project in focus"));
    assert!(section.contains("\"Storefront\" (id 12, slug `storefront`)"));
    assert!(section.contains("not a limit"));
}

#[test]
fn system_sections_are_reported_largest_first() {
    let messages = vec![
        ChatMessage::system("intro\n## Small\nab\n## Large\nabcdefghijkl\n"),
        user_msg("ignored"),
    ];
    let sizes = system_section_sizes(&messages);
    assert_eq!(sizes, "Large=13 | (preamble)=6 | Small=3");
}

#[test]
fn the_latest_round_of_results_is_never_stubbed() {
    let big = "x".repeat(4000);
    let mut messages = vec![
        ChatMessage::system("seed"),
        assistant_msg("one round, three calls"),
        ChatMessage::tool("a".to_string(), big.clone()),
        ChatMessage::tool("b".to_string(), big.clone()),
        ChatMessage::tool("c".to_string(), big.clone()),
    ];
    trim_carried_tool_results(&mut messages, 1000);
    assert!(messages[2..].iter().all(|m| m.content == big));
}

#[test]
fn trimming_leaves_a_transcript_under_budget_alone() {
    let mut messages = vec![
        ChatMessage::tool("a".to_string(), "small".to_string()),
        ChatMessage::tool("b".to_string(), "also small".to_string()),
    ];
    let before = messages.clone();
    trim_carried_tool_results(&mut messages, 192 * 1024);
    assert_eq!(messages[0].content, before[0].content);
    assert_eq!(messages[1].content, before[1].content);
}

fn user_msg(text: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: text.to_string(),
        ..Default::default()
    }
}

#[test]
fn history_within_budget_is_left_alone() {
    let mut messages = vec![ChatMessage::system("seed"), user_msg("hi")];
    assert_eq!(bound_history_for_model(&mut messages, 1024), 0);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "seed");
}

#[test]
fn history_over_budget_drops_oldest_turns_and_keeps_system_and_latest() {
    let big = "x".repeat(400);
    let mut messages = vec![
        ChatMessage::system("seed"),
        user_msg(&big),
        ChatMessage::assistant(big.clone()),
        user_msg(&big),
        user_msg("the current question"),
    ];
    let dropped = bound_history_for_model(&mut messages, 700);

    assert_eq!(dropped, 2);
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages.last().map(|m| m.content.as_str()),
        Some("the current question")
    );
    // The gap is announced inside the system message, not as a second one.
    assert_eq!(messages.iter().filter(|m| m.role == "system").count(), 1);
    assert!(messages[0].content.starts_with("seed"));
    assert!(messages[0].content.contains("2 earlier message(s)"));
    let total: usize = messages.iter().map(|m| m.content.len()).sum();
    assert!(
        total <= 700 + messages[0].content.len(),
        "still over budget: {total}"
    );
}

#[test]
fn history_never_drops_the_latest_message_even_when_it_alone_is_over_budget() {
    let mut messages = vec![ChatMessage::system("seed"), user_msg(&"y".repeat(5000))];
    assert_eq!(bound_history_for_model(&mut messages, 100), 0);
    assert_eq!(messages.len(), 2);
}

#[test]
fn only_assistant_conversations_get_the_tighter_turn_caps() {
    assert_eq!(
        max_rounds_for_context("assistant", 500),
        ASSISTANT_MAX_ROUNDS
    );
    assert_eq!(max_rounds_for_context("project", 500), 500);
    assert_eq!(max_rounds_for_context("global", 500), 500);
    assert_eq!(
        max_calls_per_round_for_context("assistant"),
        ASSISTANT_MAX_CALLS_PER_ROUND
    );
    assert_eq!(max_calls_per_round_for_context("project"), usize::MAX);
}

#[test]
fn a_capped_turn_is_told_to_wrap_up_three_rounds_before_the_cap() {
    let cap = ASSISTANT_MAX_ROUNDS;
    assert_eq!(steps_left_note(cap, cap - 3), Some(STEPS_LEFT_NOTE));
    assert_eq!(steps_left_note(cap, cap - 4), None);
    assert_eq!(steps_left_note(cap, cap - 2), None);
    assert!(STEPS_LEFT_NOTE.contains("temps_goal"));
    // The generic anti-spin ceiling is not a budget to plan by.
    assert_eq!(steps_left_note(500, 497), None);
    // Too few rounds to warn about.
    assert_eq!(steps_left_note(3, 0), None);
}

#[test]
fn the_over_cap_result_names_the_cap_and_counts_as_unproductive() {
    let message = tool_call_cap_message(8);
    assert!(message.contains('8'));
    assert!(
        !tool_result_is_productive(&message),
        "a turn that only exceeds the cap must trip the stuck-turn guard"
    );
}

/// A `create_alert` proposal with no prior backtest must be refused.
///
/// The system prompt asks for a backtest in the strongest terms available
/// and a live model still skipped it — then wrote "backtested via
/// preview_alert" in its answer without having called it. A threshold
/// nobody checked is a guess, so this is enforced rather than requested.
#[test]
fn create_alert_without_a_backtest_is_refused() {
    let seen = HashMap::new();
    let msg = missing_backtest(
        "alerts create_alert --metric_name http.server.duration --severity critical",
        &seen,
    )
    .expect("must refuse an un-backtested proposal");

    assert!(msg.contains("preview_alert"), "must name the tool: {msg}");
    assert!(
        msg.contains("Not staged"),
        "must be clear nothing was proposed: {msg}"
    );
}

/// A successful backtest earlier in the turn satisfies the requirement.
#[test]
fn create_alert_after_a_backtest_is_allowed() {
    let mut seen = HashMap::new();
    seen.insert(
        "temps|{\"command\":\"alerts preview_alert --metric_name http.server.duration\"}"
            .to_string(),
        "{\"operation\":\"preview_alert\",\"status\":200,\"data\":{\"breach_count\":22}}"
            .to_string(),
    );
    assert!(missing_backtest(
        "alerts create_alert --metric_name http.server.duration",
        &seen
    )
    .is_none());
}

/// A backtest that errored proves nothing, so it must not unlock the
/// proposal — otherwise a single malformed call becomes a way past the gate.
#[test]
fn a_failed_backtest_does_not_satisfy_the_requirement() {
    let mut seen = HashMap::new();
    seen.insert(
        "temps|{\"command\":\"alerts preview_alert --metric foo\"}".to_string(),
        "`preview_alert` failed: Unknown parameter(s) for operation 'preview_alert': 'metric'"
            .to_string(),
    );
    assert!(missing_backtest("alerts create_alert --metric_name foo", &seen).is_some());

    // ...and a non-2xx from the real operation is not a backtest either.
    let mut seen = HashMap::new();
    seen.insert(
        "k".to_string(),
        "{\"operation\":\"preview_alert\",\"status\":400,\"data\":null}".to_string(),
    );
    assert!(missing_backtest("alerts create_alert --metric_name foo", &seen).is_some());
}

/// The gate must judge what RAN, not what was asked for.
///
/// `preview_alert --help` names the operation and returns friendly text. The
/// original implementation matched on the argument string, so help counted
/// as a backtest: the model could propose a threshold it never verified
/// while the human confirming saw it described as checked. Found by an
/// independent security audit, not by the author.
#[test]
fn help_output_does_not_satisfy_the_backtest_requirement() {
    let mut seen = HashMap::new();
    seen.insert(
        "temps|{\"command\":\"alerts preview_alert --help\"}".to_string(),
        "preview_alert — POST /otel/alerts/preview\nBacktest an anomaly detector.\nFlags:\n  \
         --metric_name <string>"
            .to_string(),
    );
    assert!(
        missing_backtest("alerts create_alert --metric_name x", &seen).is_some(),
        "help text must never count as having run the backtest"
    );
}

/// Nor may a different operation that merely mentions it in its arguments.
#[test]
fn another_operation_mentioning_the_backtest_does_not_satisfy_it() {
    let mut seen = HashMap::new();
    seen.insert(
        "temps|{\"command\":\"alerts list_alerts --name preview_alert\"}".to_string(),
        "{\"operation\":\"list_alerts\",\"status\":200,\"data\":{\"data\":[]}}".to_string(),
    );
    assert!(missing_backtest("alerts create_alert --metric_name x", &seen).is_some());
}

/// Everything else stays unaffected — this gate is about thresholds, not
/// write actions in general.
#[test]
fn other_write_operations_need_no_backtest() {
    let seen = HashMap::new();
    for cmd in [
        "deployments trigger_project_pipeline --environment_id 8",
        "containers restart_container --id abc",
        "environments update_environment_settings --replicas 2",
    ] {
        assert!(
            missing_backtest(cmd, &seen).is_none(),
            "{cmd} must not require a backtest"
        );
    }
}

#[test]
fn clean_title_strips_quotes_and_punctuation() {
    assert_eq!(clean_title("\"Recent Audit Logs.\""), "Recent Audit Logs");
    assert_eq!(
        clean_title("  Deploy Failure Investigation!  "),
        "Deploy Failure Investigation"
    );
}

#[test]
fn clean_title_keeps_first_nonempty_line() {
    assert_eq!(
        clean_title("\n\n  Fetch Audit Logs\nextra line"),
        "Fetch Audit Logs"
    );
    assert_eq!(clean_title("Fetch Audit Logs\nextra"), "Fetch Audit Logs");
}

#[test]
fn clean_title_collapses_whitespace_and_caps_length() {
    assert_eq!(clean_title("Get   last    20  logs"), "Get last 20 logs");
    let long = "word ".repeat(40);
    assert!(clean_title(&long).chars().count() <= TITLE_MAX_CHARS);
}

#[test]
fn clean_title_empty_input_is_empty() {
    assert_eq!(clean_title("   \n  "), "");
}

use std::sync::Mutex;

use async_trait::async_trait;
use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

use temps_ai::{
    AiError, AiRequest, AiResponse, ChatStreamDelta, ChatTurnStream, TokenStream, ToolCall,
};

#[test]
fn assistant_draft_keeps_every_tool_and_interleaved_text_segment() {
    let mut tools = Vec::new();
    let mut parts = vec![serde_json::json!({
        "type": "text",
        "text": "Scaffolded the app."
    })];
    record_tool_call(
        &mut tools,
        &mut parts,
        "tool-1",
        "Bash",
        r#"{"command":"npm create next-app"}"#,
    );
    record_tool_result(
        &mut tools,
        &mut parts,
        "tool-1",
        "Bash",
        r#"{"command":"npm create next-app"}"#,
        "Created landing-page",
    );
    record_tool_call(
        &mut tools,
        &mut parts,
        "tool-2",
        "Bash",
        r#"{"command":"npx shadcn init"}"#,
    );

    let metadata = assistant_message_metadata(&tools, &parts, true)
        .expect("a draft with tool calls has metadata");
    let persisted_tools = metadata["tools"].as_array().expect("tools array");
    let persisted_parts = metadata["parts"].as_array().expect("parts array");

    assert_eq!(persisted_tools.len(), 2);
    assert_eq!(persisted_tools[0]["result"], "Created landing-page");
    assert!(persisted_tools[1]["result"].is_null());
    assert_eq!(persisted_parts.len(), 3);
    assert_eq!(persisted_parts[0]["text"], "Scaffolded the app.");
    assert_eq!(metadata["draft"], true);
}

#[test]
fn interrupted_turn_finishes_only_unresolved_tool_calls_in_both_replay_shapes() {
    let mut tools = Vec::new();
    let mut parts = Vec::new();
    record_tool_call(&mut tools, &mut parts, "done", "Bash", "{}");
    record_tool_result(&mut tools, &mut parts, "done", "Bash", "{}", "ok");
    record_tool_call(&mut tools, &mut parts, "pending", "Bash", "{}");

    assert_eq!(
        finish_unresolved_tool_calls(&mut tools, &mut parts),
        vec![("pending".to_string(), "Bash".to_string())]
    );
    assert_eq!(tools[0]["result"], "ok");
    assert_eq!(parts[0]["tool"]["result"], "ok");
    assert_eq!(tools[1]["result"], INTERRUPTED_TOOL_RESULT);
    assert_eq!(parts[1]["tool"]["result"], tools[1]["result"]);
    assert!(finish_unresolved_tool_calls(&mut tools, &mut parts).is_empty());
}

#[tokio::test]
async fn active_assistant_checkpoint_is_written_to_the_message_row() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let tools = vec![serde_json::json!({
        "id": "tool-1",
        "name": "Bash",
        "arguments": "{}",
        "result": "done"
    })];
    let parts = vec![serde_json::json!({
        "type": "text",
        "text": "Before approval."
    })];

    persist_assistant_message(
        &db,
        42,
        "Before approval. Continuing after approval.",
        &tools,
        &parts,
        " Continuing after approval.",
        true,
    )
    .await
    .expect("checkpoint persists");

    let log = db.into_transaction_log();
    let statements = log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .collect::<Vec<_>>();
    assert_eq!(statements.len(), 1);
    assert!(statements[0].sql.contains("UPDATE \"ai_messages\""));
    let statement = format!("{:?}", statements[0]);
    assert!(statement.contains("Before approval. Continuing after approval."));
    assert!(statement.contains("tool-1"));
    assert!(statement.contains("draft"));
}

/// A scripted `AiService`: each `chat_stream_turn` call pops the next queued
/// round (a list of [`ChatStreamDelta`]s to stream, or an error to fail the
/// call with) so a test can drive the agentic loop round-by-round, while
/// counting how many model calls were made.
struct ScriptedAi {
    /// Front-to-back queue of rounds for successive `chat_stream_turn` calls.
    rounds: Mutex<std::collections::VecDeque<Result<Vec<ChatStreamDelta>, AiError>>>,
    /// Counts `chat_stream_turn` invocations (kept named `chat_calls` for the
    /// round-cap assertions).
    chat_calls: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<Mutex<Vec<ChatTurnRequest>>>,
    available: bool,
    /// Advance the paused test clock by this much on every model call, so a
    /// deadline can be exercised without a test that actually waits.
    advance_per_round: Option<std::time::Duration>,
}

impl ScriptedAi {
    fn new(rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into_iter().collect()),
            chat_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            available: true,
            advance_per_round: None,
        }
    }

    /// Advance the paused clock by `d` on each model call.
    fn advancing(mut self, d: std::time::Duration) -> Self {
        self.advance_per_round = Some(d);
        self
    }

    fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new(vec![])
        }
    }
}

#[async_trait]
impl AiService for ScriptedAi {
    async fn is_available(&self) -> bool {
        self.available
    }
    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }
    async fn capabilities_for(
        &self,
        provider: Option<&str>,
        _refresh: temps_ai::RefreshPolicy,
    ) -> Result<temps_ai::ProviderCapabilities, AiError> {
        Ok(temps_ai::ProviderCapabilities {
            id: provider.unwrap_or("gateway_key:1").to_string(),
            name: "Test provider".to_string(),
            auth_source: temps_ai::ProviderAuthSource::ConfiguredKey,
            models: vec![
                temps_ai::ModelCapability {
                    id: "gpt-4o-mini".to_string(),
                    name: "GPT-4o mini".to_string(),
                    thinking_modes: vec![temps_ai::SelectOption {
                        id: "high".to_string(),
                        name: "High".to_string(),
                        description: None,
                    }],
                    tool_thinking_modes: None,
                    default_thinking_mode_id: None,
                },
                temps_ai::ModelCapability {
                    id: "gpt-4.1".to_string(),
                    name: "GPT-4.1".to_string(),
                    thinking_modes: vec![temps_ai::SelectOption {
                        id: "low".to_string(),
                        name: "Low".to_string(),
                        description: None,
                    }],
                    tool_thinking_modes: None,
                    default_thinking_mode_id: Some("low".to_string()),
                },
                temps_ai::ModelCapability {
                    id: "gpt-5.6-luna".to_string(),
                    name: "GPT-5.6 Luna".to_string(),
                    thinking_modes: vec![
                        temps_ai::SelectOption {
                            id: "none".to_string(),
                            name: "None".to_string(),
                            description: None,
                        },
                        temps_ai::SelectOption {
                            id: "medium".to_string(),
                            name: "Medium".to_string(),
                            description: None,
                        },
                    ],
                    tool_thinking_modes: None,
                    default_thinking_mode_id: Some("medium".to_string()),
                },
            ],
            default_model_id: Some("gpt-4o-mini".to_string()),
            permission_modes: vec![
                temps_ai::SelectOption {
                    id: "confirm-actions".to_string(),
                    name: "Confirm actions".to_string(),
                    description: None,
                },
                temps_ai::SelectOption {
                    id: "full-access".to_string(),
                    name: "Full access".to_string(),
                    description: None,
                },
            ],
            default_permission_mode_id: Some("confirm-actions".to_string()),
            realtime: temps_ai::RealtimeCapabilities {
                text_streaming: true,
                reasoning_streaming: false,
                tool_events: true,
                user_interactions: true,
                cancellation: true,
            },
        })
    }
    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
    async fn chat_stream_turn(&self, request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        self.requests.lock().unwrap().push(request);
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(d) = self.advance_per_round {
            tokio::time::advance(d).await;
        }
        // When the script is exhausted, keep requesting the same tool call so a
        // misbehaving loop would run forever — letting MAX_ROUNDS assert.
        let round = self
            .rounds
            .lock()
            .expect("scripted-ai lock")
            .pop_front()
            .unwrap_or_else(|| {
                Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                    id: "loop".to_string(),
                    name: "echo".to_string(),
                    arguments: "{}".to_string(),
                })])
            });
        let deltas = round?;
        let s = async_stream::stream! {
            for d in deltas {
                yield Ok(d);
            }
        };
        Ok(Box::pin(s))
    }
}

/// Models the common local-development setup: no gateway key is configured
/// while an authenticated host harness is ready for chat.
struct HostHarnessOnlyAi;

struct NonAuthoritativeCapabilitiesAi(temps_ai::ModelCatalogSource);

struct ToolRestrictedCapabilitiesAi;

#[async_trait]
impl AiService for NonAuthoritativeCapabilitiesAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn capabilities_snapshot_for_principal(
        &self,
        provider: Option<&str>,
        _principal_id: i32,
        _refresh: temps_ai::RefreshPolicy,
    ) -> Result<temps_ai::ProviderCapabilitiesSnapshot, AiError> {
        let capabilities = ScriptedAi::new(vec![])
            .capabilities_for(provider, temps_ai::RefreshPolicy::Cached)
            .await?;
        Ok(temps_ai::ProviderCapabilitiesSnapshot {
            capabilities,
            model_source: self.0,
            models_refreshed_at: None,
        })
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
}

#[async_trait]
impl AiService for ToolRestrictedCapabilitiesAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn capabilities_snapshot_for_principal(
        &self,
        provider: Option<&str>,
        _principal_id: i32,
        _refresh: temps_ai::RefreshPolicy,
    ) -> Result<temps_ai::ProviderCapabilitiesSnapshot, AiError> {
        let mut capabilities = ScriptedAi::new(vec![])
            .capabilities_for(provider, temps_ai::RefreshPolicy::Cached)
            .await?;
        let model = capabilities
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-4o-mini")
            .expect("test model");
        model.tool_thinking_modes = Some(vec![temps_ai::SelectOption {
            id: "medium".to_string(),
            name: "Medium".to_string(),
            description: None,
        }]);
        Ok(temps_ai::ProviderCapabilitiesSnapshot {
            capabilities,
            model_source: temps_ai::ModelCatalogSource::Live,
            models_refreshed_at: None,
        })
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
}

#[async_trait]
impl AiService for HostHarnessOnlyAi {
    async fn is_available(&self) -> bool {
        false
    }

    async fn chat_capable_for(&self, provider: Option<&str>) -> bool {
        provider == Some("claude_cli")
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
}

/// A stub provider exposing a single `echo` tool, counting executions.
struct StubProvider {
    tool_calls: Arc<std::sync::atomic::AtomicUsize>,
}

struct AuthRecordingProvider {
    seen_user_id: Arc<std::sync::atomic::AtomicI32>,
}

struct SeedOnlyProvider;

struct RevokedProjectAccessChecker;

struct ReadOnlyProjectAccessChecker;

struct RevokingProjectAccessChecker {
    checks: std::sync::atomic::AtomicUsize,
}

struct ProjectEightRevokedChecker;

#[async_trait]
impl temps_core::ProjectAccessChecker for RevokedProjectAccessChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(false)
    }
}

#[async_trait]
impl temps_core::ProjectAccessChecker for ReadOnlyProjectAccessChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(true)
    }

    async fn effective_project_permissions(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Some(vec![
            temps_auth::permissions::Permission::ProjectsRead.to_string(),
        ]))
    }
}

#[async_trait]
impl temps_core::ProjectAccessChecker for RevokingProjectAccessChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        _project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.checks.fetch_add(1, Ordering::SeqCst) == 0)
    }
}

#[async_trait]
impl temps_core::ProjectAccessChecker for ProjectEightRevokedChecker {
    async fn user_can_access_project(
        &self,
        _user_id: i32,
        project_id: i32,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(project_id != 8)
    }
}

#[async_trait]
impl ConversationContextProvider for StubProvider {
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
        vec![ChatTool {
            name: "echo".to_string(),
            description: "Echoes its input.".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }
    async fn execute_tool(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
        _name: &str,
        _arguments: &str,
    ) -> String {
        self.tool_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        "tool result".to_string()
    }
}

#[async_trait]
impl ConversationContextProvider for AuthRecordingProvider {
    fn context_type(&self) -> &'static str {
        "__api_tools__"
    }

    async fn seed(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
    ) -> Option<crate::provider::ConversationSeed> {
        None
    }

    async fn execute_tool_with_auth(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
        _name: &str,
        _arguments: &str,
        auth: &AuthContext,
    ) -> String {
        self.seen_user_id
            .store(auth.user_id(), std::sync::atomic::Ordering::SeqCst);
        "authenticated result".to_string()
    }
}

#[async_trait]
impl ConversationContextProvider for SeedOnlyProvider {
    fn context_type(&self) -> &'static str {
        "deployment"
    }

    async fn seed(
        &self,
        _project_id: Option<i32>,
        _context_id: &str,
    ) -> Option<crate::provider::ConversationSeed> {
        Some(crate::provider::ConversationSeed {
            system: "private system context".to_string(),
            first_assistant: None,
            title: Some("Private chat".to_string()),
            metadata: None,
        })
    }
}

#[tokio::test]
async fn application_runtime_gate_blocks_claims_only_for_updating_application() {
    let gate = application_runtime_gate("app_update_test");
    let update = gate.clone().try_write_owned().expect("exclusive update");
    assert!(application_runtime_gate("app_update_test")
        .try_read_owned()
        .is_err());
    assert!(application_runtime_gate("app_other_test")
        .try_read_owned()
        .is_ok());
    drop(update);
    let first = gate.clone().try_read_owned().expect("first thread");
    let second = gate.clone().try_read_owned().expect("parallel thread");
    assert!(gate.clone().try_write_owned().is_err());
    drop((first, second));
    assert!(gate.try_write_owned().is_ok());
}

#[tokio::test]
async fn runtime_update_refuses_active_threads_and_releases_gate_on_error() {
    let mut conversation = test_conversation();
    conversation.context_type = "application".into();
    conversation.context_id = "app_busy_test:thread".into();
    conversation.turn_status = "running".into();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![conversation]])
        .into_connection();
    let svc = service_with_db(Arc::new(ScriptedAi::new(vec![])), db);
    assert!(matches!(
        svc.lock_application_runtime_update("app_busy_test").await,
        Err(ChatError::WorkspaceRuntimeBusy { .. })
    ));
    assert!(application_runtime_gate("app_busy_test")
        .try_read_owned()
        .is_ok());
}

#[tokio::test]
async fn runtime_update_gate_prevents_new_claim_before_database_write() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<ai_conversations::Model>::new()])
        .into_connection();
    let svc = service_with_db(Arc::new(ScriptedAi::new(vec![])), db);
    let guard = svc
        .lock_application_runtime_update("app_claim_test")
        .await
        .expect("idle workspace");
    let mut conversation = test_conversation();
    conversation.context_type = "application".into();
    conversation.context_id = "app_claim_test:thread".into();
    assert!(matches!(
        svc.claim_turn(&conversation, "blocked-turn").await,
        Err(ChatError::WorkspaceRuntimeBusy { .. })
    ));
    drop(guard);
    assert!(application_runtime_gate("app_claim_test")
        .try_read_owned()
        .is_ok());
}

#[tokio::test]
async fn runtime_update_fails_closed_on_database_error() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("unavailable".into())])
        .into_connection();
    let svc = service_with_db(Arc::new(ScriptedAi::new(vec![])), db);
    assert!(matches!(
        svc.lock_application_runtime_update("app_db_error_test")
            .await,
        Err(ChatError::Db(_))
    ));
    assert!(application_runtime_gate("app_db_error_test")
        .try_read_owned()
        .is_ok());
}

fn test_conversation() -> ai_conversations::Model {
    let now = Utc::now();
    ai_conversations::Model {
        id: 1,
        public_id: "pub1".to_string(),
        project_id: Some(7),
        application_id: None,
        context_type: "test".to_string(),
        context_id: "42".to_string(),
        title: None,
        status: "active".to_string(),
        kind: "ai".to_string(),
        created_by: 1,
        metadata: None,
        cli_session_id: None,
        cli_session_fingerprint: None,
        ai_provider: "gateway".to_string(),
        ai_model: "gpt-4o-mini".to_string(),
        ai_thinking_level: None,
        ai_permission_mode: "confirm-actions".to_string(),
        turn_status: "idle".to_string(),
        active_turn_id: None,
        last_turn_id: None,
        turn_started_at: None,
        next_step_suggestions: None,
        created_at: now,
        last_activity_at: now,
    }
}

fn test_auth() -> AuthContext {
    let now = Utc::now();
    let user = temps_entities::users::Model {
        id: 1,
        name: "tester".to_string(),
        email: "tester@internal".to_string(),
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
    };
    AuthContext::new_session(user, temps_auth::permissions::Role::Admin)
}

#[tokio::test]
async fn tool_authorization_rejects_a_revoked_session() {
    let user = test_auth().user.expect("test user");
    let auth = AuthContext::new_persisted_session(user, Role::Admin, 41);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::sessions::Model>::new()])
        .into_connection();

    let result = refresh_tool_authorization(&db, &auth).await;
    assert!(matches!(
        result,
        Err(ToolAuthorizationRefreshError::PrincipalInactive)
    ));
}

#[tokio::test]
async fn tool_authorization_rejects_a_revoked_api_key() {
    let user = test_auth().user.expect("test user");
    let auth = AuthContext::new_api_key(user, Some(Role::Admin), None, "revoked".to_string(), 71);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
        .into_connection();

    let result = refresh_tool_authorization(&db, &auth).await;
    assert!(matches!(
        result,
        Err(ToolAuthorizationRefreshError::PrincipalInactive)
    ));
}

#[tokio::test]
async fn revoked_api_key_stops_the_turn_before_tool_dispatch() {
    let user = test_auth().user.expect("test user");
    let auth = AuthContext::new_api_key(user, Some(Role::Admin), None, "revoked".to_string(), 71);
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::ToolCall(
        ToolCall {
            id: "must-not-run".to_string(),
            name: "echo".to_string(),
            arguments: "{}".to_string(),
        },
    )])]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let tool_calls = provider.tool_calls.clone();
    let provider_calls = ai.chat_calls.clone();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
        .into_connection();
    let service = service_with_db(ai, db);
    let provider: Arc<dyn ConversationContextProvider> = provider;
    let mut stream = service
        .try_tool_loop(
            &test_conversation(),
            vec![],
            Some(provider),
            echo_tools(),
            &auth,
        )
        .await;

    let first = stream.next().await.expect("provider tool-call event");
    assert!(matches!(first, Ok(ChatStreamEvent::ToolCall { .. })));
    let error = stream
        .next()
        .await
        .expect("authorization failure event")
        .expect_err("revoked key must fail before dispatch");
    assert!(matches!(
        error,
        ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::PrincipalInactive)
    ));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn authorization_revocation_stops_an_active_native_provider_stream() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 74,
        name: "active-turn".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "admin".to_string(),
        permissions: None,
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let auth = AuthContext::new_api_key(
        user.clone(),
        Some(Role::Admin),
        None,
        key.name.clone(),
        key.id,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ai = Arc::new(PendingStreamAi {
        calls: calls.clone(),
    });
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[key]])
        .append_query_results([[user]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .append_query_results([Vec::<temps_entities::api_keys::Model>::new()])
        .into_connection();
    let service = service_with_db(ai, db);
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    let mut stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            Some(provider),
            echo_tools(),
            &auth,
            &test_request_metadata(),
            None,
            Vec::new(),
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_auth_monitor".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_auth_monitor"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await;

    let error = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("authorization monitor must stop the provider promptly")
        .expect("authorization failure event")
        .expect_err("revoked key must fail the active turn");
    assert!(matches!(
        error,
        ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::PrincipalInactive)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn project_revocation_stops_an_active_native_provider_stream() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 75,
        name: "project-turn".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "custom".to_string(),
        permissions: Some(
            "[\"projects:read\",\"projects:write\",\"sandboxes:write\",\"ai_gateway:write\"]"
                .to_string(),
        ),
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let auth = AuthContext::new_api_key(
        user.clone(),
        None,
        Some(vec![
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
            Permission::AiGatewayWrite,
        ]),
        key.name.clone(),
        key.id,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ai = Arc::new(PendingStreamAi {
        calls: calls.clone(),
    });
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[key.clone()]])
        .append_query_results([[user.clone()]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .append_query_results([[key]])
        .append_query_results([[user]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .into_connection();
    let service = service_with_db(ai, db);
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let checker: Arc<dyn temps_core::ProjectAccessChecker> =
        Arc::new(RevokingProjectAccessChecker {
            checks: std::sync::atomic::AtomicUsize::new(0),
        });
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    let mut stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            Some(provider),
            echo_tools(),
            &auth,
            &test_request_metadata(),
            Some(checker),
            vec![7],
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_project_monitor".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_project_monitor"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await;

    let error = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("project monitor must stop the provider promptly")
        .expect("project authorization failure event")
        .expect_err("revoked project membership must fail the active turn");
    assert!(matches!(
        error,
        ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::HarnessPermissionRevoked)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runtime_permission_downgrade_stops_an_active_native_provider_stream() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let active_key = temps_entities::api_keys::Model {
        id: 76,
        name: "runtime-turn".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "custom".to_string(),
        permissions: Some(
            "[\"projects:read\",\"sandboxes:write\",\"ai_gateway:write\"]".to_string(),
        ),
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let mut downgraded_key = active_key.clone();
    downgraded_key.permissions = Some("[\"projects:read\",\"sandboxes:write\"]".to_string());
    let auth = AuthContext::new_api_key(
        user.clone(),
        None,
        Some(vec![
            Permission::ProjectsRead,
            Permission::SandboxesWrite,
            Permission::AiGatewayWrite,
        ]),
        active_key.name.clone(),
        active_key.id,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ai = Arc::new(PendingStreamAi {
        calls: calls.clone(),
    });
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[active_key]])
        .append_query_results([[user.clone()]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .append_query_results([[downgraded_key]])
        .append_query_results([[user]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .into_connection();
    let service = service_with_db(ai, db);
    let mut conversation = test_conversation();
    conversation.context_type = "global".to_string();
    conversation.project_id = None;
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    let mut stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &auth,
            &test_request_metadata(),
            None,
            Vec::new(),
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "global_runtime_monitor".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/global_runtime_monitor"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await;

    let error = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("runtime monitor must stop the provider promptly")
        .expect("runtime authorization failure event")
        .expect_err("losing provider access must fail the active turn");
    assert!(matches!(
        error,
        ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::HarnessPermissionRevoked)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn newly_linked_project_is_authorized_during_an_active_native_provider_stream() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 77,
        name: "topology-turn".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "custom".to_string(),
        permissions: Some(
            "[\"projects:read\",\"projects:write\",\"sandboxes:write\",\"ai_gateway:write\"]"
                .to_string(),
        ),
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let auth = AuthContext::new_api_key(
        user.clone(),
        None,
        Some(vec![
            Permission::ProjectsRead,
            Permission::ProjectsWrite,
            Permission::SandboxesWrite,
            Permission::AiGatewayWrite,
        ]),
        key.name.clone(),
        key.id,
    );
    let application = temps_entities::ai_applications::Model {
        id: 99,
        public_id: "app-topology".to_string(),
        name: "Topology app".to_string(),
        description: None,
        status: "active".to_string(),
        created_by: user.id,
        created_at: now,
        updated_at: now,
    };
    let project_seven = temps_entities::ai_application_projects::Model {
        id: 1,
        application_id: application.id,
        project_id: 7,
        is_primary: true,
        created_at: now,
    };
    let project_eight = temps_entities::ai_application_projects::Model {
        id: 2,
        application_id: application.id,
        project_id: 8,
        is_primary: false,
        created_at: now,
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ai = Arc::new(PendingStreamAi {
        calls: calls.clone(),
    });
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[key.clone()]])
        .append_query_results([[user.clone()]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .append_query_results([[application.clone()]])
        .append_query_results([[project_seven.clone()]])
        .append_query_results([[key]])
        .append_query_results([[user]])
        .append_query_results([[owner_admin_role()]])
        .append_query_results([[owner_admin_grant()]])
        .append_query_results([[application]])
        .append_query_results([[project_seven, project_eight]])
        .into_connection();
    let service = service_with_db(ai, db);
    let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(ProjectEightRevokedChecker);
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.application_id = Some(99);
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    let mut stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &auth,
            &test_request_metadata(),
            Some(checker),
            vec![7],
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_topology_monitor".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_topology_monitor"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await;

    let error = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("topology monitor must stop the provider promptly")
        .expect("new project authorization failure event")
        .expect_err("an inaccessible newly linked project must fail the active turn");
    assert!(matches!(
        error,
        ChatError::AuthorizationRefresh(ToolAuthorizationRefreshError::HarnessPermissionRevoked)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn harness_runtime_authorization_rejects_unknown_and_elevated_modes() {
    let user = test_auth().user.expect("test user");
    let auth = AuthContext::new_api_key(
        user,
        None,
        Some(vec![Permission::AiGatewayWrite]),
        "runtime".to_string(),
        78,
    );

    assert!(harness_runtime_authorized(&auth, "claude_cli", "default"));
    assert!(!harness_runtime_authorized(
        &auth,
        "claude_cli",
        "full-access"
    ));
    assert!(!harness_runtime_authorized(&auth, "claude_cli", "unknown"));
    assert!(!harness_runtime_authorized(&auth, "codex_cli", "auto"));
}

#[tokio::test]
async fn tool_authorization_uses_current_api_key_permissions() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 72,
        name: "current".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "custom".to_string(),
        permissions: Some("[\"projects:read\"]".to_string()),
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let captured = AuthContext::new_api_key(
        user.clone(),
        None,
        Some(vec![Permission::ProjectsWrite]),
        key.name.clone(),
        key.id,
    );
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[key]])
        .append_query_results([[user]])
        // Owner's current role: no `admin` role row, so a plain user.
        .append_query_results([Vec::<temps_entities::roles::Model>::new()])
        .into_connection();

    let refreshed = refresh_tool_authorization(&db, &captured)
        .await
        .expect("current API-key authorization");
    assert!(refreshed.has_permission(&Permission::ProjectsRead));
    assert!(!refreshed.has_permission(&Permission::ProjectsWrite));
}

#[tokio::test]
async fn tool_authorization_caps_an_admin_key_whose_owner_was_demoted() {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 73,
        name: "former-admin".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "admin".to_string(),
        permissions: None,
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let captured = AuthContext::new_api_key(
        user.clone(),
        Some(Role::Admin),
        None,
        key.name.clone(),
        key.id,
    );
    let admin_role = temps_entities::roles::Model {
        id: 1,
        name: "admin".to_string(),
        created_at: now,
        updated_at: now,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[key]])
        .append_query_results([[user]])
        .append_query_results([[admin_role]])
        // The owner no longer holds the admin role.
        .append_query_results([Vec::<temps_entities::user_roles::Model>::new()])
        .into_connection();

    let refreshed = refresh_tool_authorization(&db, &captured)
        .await
        .expect("the key still authorizes as its owner");
    assert_ne!(refreshed.effective_role, Role::Admin);
    assert!(!refreshed.has_permission(&Permission::UsersWrite));
}

fn test_request_metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: "127.0.0.1".to_string(),
        user_agent: "temps-ai-chat-test".to_string(),
        headers: axum::http::HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

fn auth_with_role(role: temps_auth::permissions::Role) -> AuthContext {
    let mut auth = test_auth();
    auth.effective_role = role;
    auth
}

/// The chat endpoints require only `ProjectsRead`/`ProjectsWrite`, but the
/// repo tools read private source through the project's stored Git token
/// and stream it back verbatim in a `tool_result` frame. Without the
/// repository permission the caller must not reach them.
#[test]
fn repo_tools_require_the_git_repository_read_permission() {
    use temps_auth::permissions::{Permission, Role};

    // Admin holds everything, including GitRepositoriesRead.
    assert!(caller_may_use_repo_tools(&auth_with_role(Role::Admin)));

    // A role that can read projects but not repositories must be refused —
    // otherwise `projects:read` silently grants source-code access.
    let reader = auth_with_role(Role::Reader);
    if !reader.has_permission(&Permission::GitRepositoriesRead) {
        assert!(
            !caller_may_use_repo_tools(&reader),
            "a caller without {} must not reach the repo tools",
            Permission::GitRepositoriesRead
        );
    }

    // A custom API key scoped to projects only is the concrete case from
    // the report: ProjectsWrite, no Git permission at all.
    let mut custom = auth_with_role(Role::Custom);
    custom.custom_permissions = Some(vec![Permission::ProjectsRead, Permission::ProjectsWrite]);
    assert!(!caller_may_use_repo_tools(&custom));

    // The same key with the repository permission added is allowed.
    custom.custom_permissions = Some(vec![
        Permission::ProjectsRead,
        Permission::ProjectsWrite,
        Permission::GitRepositoriesRead,
    ]);
    assert!(caller_may_use_repo_tools(&custom));
}

#[tokio::test]
async fn revoked_project_membership_denies_repo_tool_offer_and_execution() {
    use temps_auth::permissions::{Permission, Role};

    let mut auth = auth_with_role(Role::Custom);
    auth.custom_permissions = Some(vec![Permission::GitRepositoriesRead]);
    let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(RevokedProjectAccessChecker);

    assert!(
        !project_repo_tools_allowed(&auth, Some(7), Some(&checker)).await,
        "a former project member must not be offered private repository tools"
    );

    let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: tool_calls.clone(),
    });
    let mut execution = ToolExecutionState::default();
    let result = dispatch_conversation_tool(
        &ToolCall {
            id: "revoked-repo-read".to_string(),
            name: "read_repo_file".to_string(),
            arguments: r#"{"path":"private.txt"}"#.to_string(),
        },
        Some(7),
        1,
        "42",
        &auth,
        Some(&checker),
        None,
        None,
        Some(&provider),
        None,
        None,
        None,
        None,
        &mut execution,
    )
    .await;

    assert!(result.contains("current project access"), "{result}");
    assert_eq!(
        tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "dispatch must re-check live access before invoking the repository provider"
    );
}

#[tokio::test]
async fn project_role_without_git_read_denies_repo_tools() {
    use temps_auth::permissions::{Permission, Role};

    let mut auth = auth_with_role(Role::Custom);
    auth.custom_permissions = Some(vec![Permission::GitRepositoriesRead]);
    let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(ReadOnlyProjectAccessChecker);

    assert!(
        !project_repo_tools_allowed(&auth, Some(7), Some(&checker)).await,
        "project membership must not restore Git access removed by the project role"
    );
}

fn assistant_msg_model() -> ai_messages::Model {
    ai_messages::Model {
        id: 1,
        conversation_id: 1,
        role: "assistant".to_string(),
        content: "final answer".to_string(),
        metadata: None,
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: Utc::now(),
    }
}

fn service_with_db(ai: Arc<dyn AiService>, db: DatabaseConnection) -> ConversationService {
    ConversationService {
        db: Arc::new(db),
        ai,
        providers: HashMap::new(),
        write_support: None,
        goal_support: None,
        secret_support: None,
        config: None,
        application_workspaces: None,
        application_sandboxes: None,
        source_drop_deployer: Default::default(),
        application_service: None,
        blueprint_support: None,
        pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
        harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
    }
}

fn echo_tools() -> Vec<ChatTool> {
    let tools = vec![ChatTool {
        name: "echo".to_string(),
        description: "Echoes its input.".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }];
    tools
}

/// Build a service whose only DB interaction (the final assistant insert)
/// is satisfied by one mocked query result. The provider is passed directly
/// to `try_tool_loop` per test.
fn service_with(ai: Arc<dyn AiService>) -> (ConversationService, Vec<ChatTool>) {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![assistant_msg_model()]])
        .into_connection();
    (service_with_db(ai, db), echo_tools())
}

/// Tool-loop tests use the same durable API-key identity as production.
/// The mock rows prove authorization is refreshed before the dispatch; a
/// request-only test session would now (correctly) fail closed.
/// The `admin` role row `current_user_role` looks up when a refresh
/// re-checks the key owner's role.
fn owner_admin_role() -> temps_entities::roles::Model {
    let now = Utc::now();
    temps_entities::roles::Model {
        id: 1,
        name: "admin".to_string(),
        created_at: now,
        updated_at: now,
    }
}

/// The test user's grant of [`owner_admin_role`]: the key owner is still
/// an admin, so the key keeps its full grant.
fn owner_admin_grant() -> temps_entities::user_roles::Model {
    let now = Utc::now();
    temps_entities::user_roles::Model {
        id: 1,
        user_id: test_auth().user.expect("test user").id,
        role_id: 1,
        created_at: now,
        updated_at: now,
    }
}

fn service_with_current_tool_auth(
    ai: Arc<dyn AiService>,
) -> (ConversationService, Vec<ChatTool>, AuthContext) {
    service_with_current_tool_auth_and_session_clear(ai, None)
}

fn service_with_current_tool_auth_and_session_clear(
    ai: Arc<dyn AiService>,
    missing_session_conversation: Option<ai_conversations::Model>,
) -> (ConversationService, Vec<ChatTool>, AuthContext) {
    let now = Utc::now();
    let user = test_auth().user.expect("test user");
    let key = temps_entities::api_keys::Model {
        id: 73,
        name: "tool-loop".to_string(),
        key_hash: "hash".to_string(),
        key_prefix: "tk_test_".to_string(),
        user_id: user.id,
        role_type: "admin".to_string(),
        permissions: None,
        is_active: true,
        expires_at: Some(now + chrono::Duration::hours(1)),
        last_used_at: None,
        created_at: now,
        updated_at: now,
        service_id: None,
    };
    let auth = AuthContext::new_api_key(
        user.clone(),
        Some(Role::Admin),
        None,
        key.name.clone(),
        key.id,
    );
    let mut db = MockDatabase::new(DatabaseBackend::Postgres);
    // Match the production turn backstop so long-running loop tests never
    // fall through to an unauthenticated fixture halfway through a turn.
    for index in 0..500 {
        db = db
            .append_query_results([[key.clone()]])
            .append_query_results([[user.clone()]])
            .append_query_results([[owner_admin_role()]])
            .append_query_results([[owner_admin_grant()]]);
        if index == 0 {
            if let Some(conversation) = missing_session_conversation.as_ref() {
                db = db.append_query_results([[conversation.clone()]]);
            }
        }
    }
    if missing_session_conversation.is_some() {
        db = db.append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }]);
    }
    let db = db
        .append_query_results([[assistant_msg_model()]])
        .into_connection();
    (service_with_db(ai, db), echo_tools(), auth)
}

async fn drain(
    stream: Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>,
) -> Vec<ChatStreamEvent> {
    let mut s = stream;
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        if let Ok(ev) = item {
            out.push(ev);
        }
    }
    out
}

struct StreamErrorAi;

struct MissingSessionStreamAi {
    requests: Arc<Mutex<Vec<ChatTurnRequest>>>,
}

#[async_trait]
impl AiService for MissingSessionStreamAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn(&self, request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        let first = requests.len() == 1;
        drop(requests);
        if first {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ChatStreamDelta::SessionMetadata {
                    session_id: Some("old-session".to_string()),
                    title: Some("Stale title".to_string()),
                }),
                Err(AiError::Provider {
                    purpose: "chat.application.tools".to_string(),
                    reason: "Thread not found for id old-session".to_string(),
                }),
            ])))
        } else {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ChatStreamDelta::SessionMetadata {
                    session_id: Some("new-session".to_string()),
                    title: None,
                }),
                Ok(ChatStreamDelta::Text("Stream recovered".to_string())),
            ])))
        }
    }
}

struct PendingStreamAi {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl AiService for PendingStreamAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn(&self, _request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures::stream::pending()))
    }
}

#[async_trait]
impl AiService for StreamErrorAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn(&self, _request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let stream = futures::stream::iter(vec![Err(AiError::Provider {
            purpose: "chat.test.tools".to_string(),
            reason: "Token refresh failed: 401".to_string(),
        })]);
        Ok(Box::pin(stream))
    }
}

struct NativeToolThenErrorAi {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl AiService for NativeToolThenErrorAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn(&self, _request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            let stream = async_stream::stream! {
                yield Ok(ChatStreamDelta::ToolCall(ToolCall {
                    id: "native-1".to_string(),
                    name: "Bash".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                }));
                yield Err(AiError::Provider {
                    purpose: "chat.application.tools".to_string(),
                    reason: "native permission bridge unavailable".to_string(),
                });
            };
            Ok(Box::pin(stream))
        } else {
            let stream = async_stream::stream! {
                yield Ok(ChatStreamDelta::Text("Could not finish the native command.".to_string()));
            };
            Ok(Box::pin(stream))
        }
    }
}

/// Concatenate every `Token` event's text, in order.
fn joined_text(events: &[ChatStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn public_tool_results_hide_privileged_read_data() {
    assert_eq!(
        public_tool_result("read_repo_file", "SECRET_TOKEN=abc123"),
        "Tool completed; detailed result is withheld from the chat transcript."
    );
    assert_eq!(
        public_tool_result(TEMPS_WRITE_TOOL_NAME, r#"{"status":"proposed"}"#),
        r#"{"status":"proposed"}"#
    );
    assert_eq!(
        public_tool_result("mcp__temps-chat__temps_write", r#"{"status":"proposed"}"#),
        r#"{"status":"proposed"}"#
    );
}

#[test]
fn title_source_keeps_other_metadata_and_marks_user_renames() {
    let existing = Some(serde_json::json!({ "seed": { "log_id": 7 } }));
    let marked = with_title_source(existing, "user");
    assert_eq!(marked["seed"]["log_id"], 7);
    assert!(title_was_set_by_user(Some(&marked)));
    assert!(!title_was_set_by_user(Some(&with_title_source(
        None, "auto"
    ))));
    assert!(!title_was_set_by_user(None));
    // A scalar left in the column is replaced rather than crashing a rename.
    let replaced = with_title_source(Some(serde_json::json!("legacy")), "user");
    assert!(title_was_set_by_user(Some(&replaced)));
}

#[test]
fn conversation_digest_prefers_recent_messages_and_skips_tool_rows() {
    let long = "x".repeat(TITLE_DIGEST_MESSAGE_CHARS * 3);
    let rows = [
        ("user", "hi"),
        ("tool", "ignored"),
        ("assistant", long.as_str()),
        ("user", "set up backups to S3"),
    ];
    let digest = conversation_digest(rows.iter().copied());
    assert!(!digest.contains("ignored"));
    assert!(digest.ends_with("User: set up backups to S3"));
    assert!(digest
        .lines()
        .all(|l| l.chars().count() <= TITLE_DIGEST_MESSAGE_CHARS + 12));

    let many: Vec<(&str, String)> = (0..100)
        .map(|i| ("user", format!("message number {i} {}", "y".repeat(300))))
        .collect();
    let digest = conversation_digest(many.iter().map(|(r, c)| (*r, c.as_str())));
    assert!(digest.chars().count() <= TITLE_DIGEST_TOTAL_CHARS);
    assert!(digest.contains("message number 99"));
    assert!(!digest.contains("message number 0 "));
    assert_eq!(conversation_digest(std::iter::empty()), "");
}

#[test]
fn api_tool_results_are_shown_redacted_and_capped() {
    let shown = public_tool_result(
        "temps",
        r#"{"status":200,"body":{"name":"web","api_key":"abc123","note":"token=hunter2"}}"#,
    );
    assert!(shown.contains(r#""name":"web""#), "{shown}");
    assert!(!shown.contains("abc123"), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");

    let huge = format!(
        r#"{{"body":"{}"}}"#,
        "é".repeat(MAX_PUBLIC_API_RESULT_BYTES)
    );
    let capped = public_tool_result("temps", &huge);
    assert!(capped.len() < MAX_PUBLIC_API_RESULT_BYTES + 100);
    assert!(
        capped.contains("truncated"),
        "{}",
        &capped[capped.len() - 60..]
    );
}

#[test]
fn proposal_claim_requires_a_fresh_write_receipt_from_this_turn() {
    let qualified_receipt = serde_json::json!({
        "id": "write-1",
        "name": "mcp__temps-chat__temps_write",
        "arguments": "{}",
        "result": r#"{"status":"proposed","action_id":"action-1"}"#,
    });
    let rejected_attempt = serde_json::json!({
        "id": "write-2",
        "name": "temps_write",
        "arguments": "{}",
        "result": "Could not stage this change: invalid version",
    });

    assert!(claims_proposal_was_staged(
        "Proposal staged — not executed. Please confirm it in the UI."
    ));
    assert!(!claims_proposal_was_staged(
        "The phrase ‘proposal staged’ means that no action has run yet."
    ));
    assert!(has_fresh_proposal_receipt(&[qualified_receipt]));
    assert!(!has_fresh_proposal_receipt(&[rejected_attempt]));
    assert!(!has_fresh_proposal_receipt(&[serde_json::json!({
        "id": "read-1",
        "name": "untrusted__temps_write",
        "result": r#"{"status":"proposed","action_id":"fake"}"#,
    })]));
}

#[tokio::test]
async fn unbacked_proposal_claim_fails_the_turn_instead_of_implying_a_card_exists() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
        "Proposal staged — not executed. Please confirm it in the UI.".to_string(),
    )])]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools, auth) = service_with_current_tool_auth(ai);
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let mut stream = svc
        .try_tool_loop(
            &test_conversation(),
            vec![],
            Some(provider_dyn),
            tools,
            &auth,
        )
        .await;

    let mut errors = Vec::new();
    while let Some(item) = stream.next().await {
        if let Err(error) = item {
            errors.push(error);
        }
    }

    assert_eq!(errors.len(), 1);
    assert!(matches!(&errors[0], ChatError::ProposalNotStaged));
    let failure = errors[0].public_failure();
    assert_eq!(failure.code, "proposal_not_staged");
    assert!(failure.detail.contains("no approval card exists"));
    assert!(failure.detail.contains("no change was made"));
}

#[tokio::test]
async fn first_application_turn_persists_and_publishes_harness_session_title() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![
        ChatStreamDelta::SessionMetadata {
            session_id: Some("session-1".to_string()),
            title: Some("Create MongoDB Instance".to_string()),
        },
        ChatStreamDelta::Text("Ready.".to_string()),
    ])]));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
            MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            },
        ])
        .append_query_results(vec![vec![assistant_msg_model()]])
        .into_connection();
    let svc = ConversationService {
        db: Arc::new(db),
        ai,
        providers: HashMap::new(),
        write_support: None,
        goal_support: None,
        secret_support: None,
        config: None,
        application_workspaces: None,
        application_sandboxes: None,
        source_drop_deployer: Default::default(),
        application_service: None,
        blueprint_support: None,
        pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
        harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
    };
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    conversation.title = Some("test-nextjs".to_string());
    let mut live = svc.subscribe_conversation(conversation.id);

    let stream = svc
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &test_auth(),
            &test_request_metadata(),
            None,
            Vec::new(),
            None,
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            true,
            None,
        )
        .await;
    drain(stream).await;

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let event = live.recv().await.expect("live event");
            if event.event == "conversation_title" {
                break event;
            }
        }
    })
    .await
    .expect("title event timeout");
    assert_eq!(event.event, "conversation_title");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&event.data).expect("title JSON")["title"],
        "Create MongoDB Instance"
    );
}

// (a) a round calls a tool, the next round answers in prose -> the tool is
// executed (ToolCall -> ToolResult, live) and the prose streams as the answer.
#[tokio::test]
async fn test_tool_loop_executes_tool_then_returns_prose() {
    let ai = Arc::new(ScriptedAi::new(vec![
        // Round 1: the model streams a tool call.
        Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
            id: "c1".to_string(),
            name: "echo".to_string(),
            arguments: "{}".to_string(),
        })]),
        // Round 2: the model answers in prose (streamed in two deltas).
        Ok(vec![
            ChatStreamDelta::Text("final ".to_string()),
            ChatStreamDelta::Text("answer".to_string()),
        ]),
    ]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let tool_count = provider.tool_calls.clone();
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    let out = drain(stream).await;

    // The tool call surfaces as ToolCall -> ToolResult (live), then the final
    // prose streams token-by-token.
    assert_eq!(
        out,
        vec![
            ChatStreamEvent::ToolCall {
                id: "c1".to_string(),
                name: "echo".to_string(),
                arguments: "{}".to_string(),
            },
            ChatStreamEvent::ToolResult {
                id: "c1".to_string(),
                name: "echo".to_string(),
                content: "Tool completed; detailed result is withheld from the chat transcript."
                    .to_string(),
            },
            ChatStreamEvent::Token("final ".to_string()),
            ChatStreamEvent::Token("answer".to_string()),
        ]
    );
    assert_eq!(tool_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(chat_count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

// (a2) a plain conversational turn streams multiple text deltas as separate
// tokens from a single model call (true token streaming, no tools).
#[tokio::test]
async fn test_tool_loop_streams_plain_answer_token_by_token() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![
        ChatStreamDelta::Text("Hello, ".to_string()),
        ChatStreamDelta::Text("world".to_string()),
    ])]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    let out = drain(stream).await;

    assert_eq!(
        out,
        vec![
            ChatStreamEvent::Token("Hello, ".to_string()),
            ChatStreamEvent::Token("world".to_string()),
        ]
    );
    // One model call only — no separate gather pass.
    assert_eq!(chat_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

// (b) the model call errors on round 1 -> the loop ends with no output.
#[tokio::test]
async fn test_tool_loop_call_error_yields_no_output() {
    let ai = Arc::new(ScriptedAi::new(vec![Err(AiError::Provider {
        purpose: "chat.test.tools".to_string(),
        reason: "boom".to_string(),
    })]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools) = service_with(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
        .await;
    let out = drain(stream).await;
    assert!(
        out.is_empty(),
        "errored call with no tools -> nothing; got {out:?}"
    );
}

#[tokio::test]
async fn test_tool_loop_surfaces_provider_error_received_inside_stream() {
    let ai: Arc<dyn AiService> = Arc::new(StreamErrorAi);
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools) = service_with(ai);

    let mut stream = svc
        .try_tool_loop(
            &test_conversation(),
            vec![],
            Some(provider),
            tools,
            &test_auth(),
        )
        .await;
    let error = stream
        .next()
        .await
        .expect("the empty turn should report its provider failure")
        .expect_err("the event should be an error");

    assert!(
        error.to_string().contains("Token refresh failed: 401"),
        "the concrete streamed provider error must reach the user: {error}"
    );
    assert!(
        !error
            .to_string()
            .contains("Check the provider's key and model"),
        "sandbox and harness failures must not be mislabeled as API key/model failures: {error}"
    );
}

#[tokio::test]
async fn native_tool_provider_error_is_not_reported_as_round_backstop_exhaustion() {
    let ai: Arc<dyn AiService> = Arc::new(NativeToolThenErrorAi {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (svc, tools, auth) = service_with_current_tool_auth(ai);
    let mut conversation = test_conversation();
    conversation.context_type = "application".to_string();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_permission_mode = "default".to_string();
    let stream = svc
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            tools,
            &auth,
            &test_request_metadata(),
            None,
            Vec::new(),
            Some(temps_ai::HarnessWorkspace {
                sandbox_label: "app_test".to_string(),
                host_work_dir: std::path::PathBuf::from("/tmp/app_test"),
            }),
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            None,
            false,
            None,
        )
        .await;
    let out = drain(stream).await;

    assert!(out.iter().any(|event| matches!(
        event,
        ChatStreamEvent::ToolCall { name, .. } if name == "Bash"
    )));
    assert!(out.iter().any(|event| matches!(
        event,
        ChatStreamEvent::Token(text) if text == "Could not finish the native command."
    )));
    assert!(
        !out.iter().any(|event| matches!(
            event,
            ChatStreamEvent::Token(text)
                if text.contains("stopped after an unusually long run of steps")
        )),
        "a provider failure after one native tool must not be described as exhausting 500 rounds: {out:?}"
    );
}

// (c) A model stuck repeating itself is stopped by the unproductive-round
// detector, long before the runaway backstop — and the user is told why.
#[tokio::test]
async fn test_tool_loop_stops_a_stuck_model_quickly() {
    // Empty script: the exhausted fallback always re-issues the SAME tool
    // call, so every round after the first is answered by the repeat guard
    // and produces nothing usable. That is the definition of stuck.
    let ai = Arc::new(ScriptedAi::new(vec![]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    let out = drain(stream).await;

    // Stopped in a handful of rounds, NOT at the 40-round backstop. The
    // backstop exists for a loop nobody is watching; a user staring at a
    // model repeating itself should not wait that long.
    let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        calls <= 6,
        "a stuck model must be stopped quickly, took {calls} rounds"
    );

    // And it must say so. A turn that halts without explanation is
    // indistinguishable from one that simply gave up.
    assert!(
        joined_text(&out).contains("produced only errors"),
        "the user must be told why it stopped; got {out:?}"
    );
}

/// A model making genuine progress is NOT cut off by a step count.
///
/// This is the property the product wants: ask the chat something and it
/// works the problem, rather than stopping every N turns. The old 10-step
/// cap ended real alert-suggestion turns with nothing proposed; steps no
/// longer govern at all.
#[tokio::test]
async fn test_tool_loop_does_not_cut_off_a_productive_model() {
    // 60 rounds, each a DIFFERENT call, so the repeat guard never fires and
    // every round counts as progress. Comfortably past any old step cap.
    let rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>> = (0..60)
        .map(|i| {
            Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                id: format!("c{i}"),
                name: "echo".to_string(),
                arguments: format!("{{\"n\":{i}}}"),
            })])
        })
        .collect();
    let ai = Arc::new(ScriptedAi::new(rounds));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    drain(stream).await;

    let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        calls >= 60,
        "a productive model must run its course, was stopped after {calls} rounds"
    );
}

/// Time is the limit. An unattended turn ends on the deadline however many
/// steps it has taken, and says so.
#[tokio::test(start_paused = true)]
async fn test_tool_loop_ends_on_the_deadline() {
    // Never-ending script; each call advances the paused clock by a minute,
    // so the 15-minute deadline trips without the test waiting.
    let ai = Arc::new(
        ScriptedAi::new(
            (0..1000)
                .map(|i| {
                    Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                        id: format!("c{i}"),
                        name: "echo".to_string(),
                        arguments: format!("{{\"n\":{i}}}"),
                    })])
                })
                .collect(),
        )
        .advancing(std::time::Duration::from_secs(60)),
    );
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    let out = drain(stream).await;

    let calls = chat_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        (14..=18).contains(&calls),
        "should stop around the 15-minute mark, stopped after {calls} rounds"
    );
    assert!(
        joined_text(&out).contains("time limit for a single turn"),
        "the user must be told time ran out; got {out:?}"
    );
}

// (c2) Salvage: after the loop exhausts MAX_ROUNDS still wanting tools, one
// final tool-free call lets the model answer from the gathered evidence.
#[tokio::test]
async fn test_tool_loop_salvages_evidence_after_round_cap() {
    // MAX_ROUNDS tool-call rounds (never settles on prose), then the tool-free
    // salvage call finally answers. Each round uses a DISTINCT argument so the
    // anti-repeat dedup guard doesn't short-circuit it — we're exercising the
    // round cap here, not the repeat guard.
    let mut rounds: Vec<Result<Vec<ChatStreamDelta>, AiError>> = (0..10)
        .map(|i| {
            Ok(vec![ChatStreamDelta::ToolCall(ToolCall {
                id: format!("c{i}"),
                name: "echo".to_string(),
                arguments: format!("{{\"n\":{i}}}"),
            })])
        })
        .collect();
    rounds.push(Ok(vec![ChatStreamDelta::Text(
        "salvaged answer".to_string(),
    )]));
    let ai = Arc::new(ScriptedAi::new(rounds));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let chat_count = ai.chat_calls.clone();
    let (svc, tools, auth) = service_with_current_tool_auth(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &auth)
        .await;
    let out = drain(stream).await;

    assert_eq!(
        joined_text(&out),
        "salvaged answer",
        "the salvage call's prose should be streamed; got {out:?}"
    );
    assert_eq!(
        chat_count.load(std::sync::atomic::Ordering::SeqCst),
        11,
        "10 tool rounds + 1 salvage call"
    );
}

// (d) a round that streams only empty text -> no answer text is produced.
#[tokio::test]
async fn test_tool_loop_empty_final_text_yields_no_text() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
        String::new(),
    )])]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools) = service_with(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
        .await;
    let out = drain(stream).await;
    assert!(joined_text(&out).is_empty());
}

#[tokio::test]
async fn temps_tool_uses_the_current_turns_auth_context() {
    let seen_user_id = Arc::new(std::sync::atomic::AtomicI32::new(-1));
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(AuthRecordingProvider {
        seen_user_id: seen_user_id.clone(),
    });
    let auth = test_auth();
    let mut execution = ToolExecutionState::default();
    let result = dispatch_conversation_tool(
        &ToolCall {
            id: "auth-call".to_string(),
            name: "temps".to_string(),
            arguments: r#"{"command":"projects get_projects"}"#.to_string(),
        },
        Some(7),
        1,
        "42",
        &auth,
        None,
        None,
        Some(&provider),
        None,
        None,
        None,
        None,
        None,
        &mut execution,
    )
    .await;

    assert_eq!(result, "authenticated result");
    assert_eq!(
        seen_user_id.load(std::sync::atomic::Ordering::SeqCst),
        auth.user_id(),
        "API tools must receive the request user's AuthContext, never server or creator auth"
    );
}

/// An interaction executor that records the request it was shown and
/// answers it with `decision`, or fails like a stopped turn when `None`.
fn scripted_interactions(
    decision: Option<PermissionDecision>,
    seen: Arc<Mutex<Vec<PermissionRequest>>>,
) -> PlatformInteractionExecutor {
    Arc::new(move |request| {
        if let Ok(mut seen) = seen.lock() {
            seen.push(request);
        }
        let decision = decision.clone();
        Box::pin(async move {
            match decision {
                Some(decision) => Ok(PermissionResolution {
                    decision,
                    auth: test_auth(),
                    metadata: test_request_metadata(),
                }),
                None => Err(temps_ai::AiError::Provider {
                    purpose: "chat.permission".to_string(),
                    reason: "turn stopped".to_string(),
                }),
            }
        })
    })
}

/// After a proposal, the rest of the turn may read and revise the
/// proposal, but never build: the user decides first.
#[tokio::test]
async fn a_turn_that_proposed_an_architecture_no_longer_builds() {
    let mut state = ToolExecutionState {
        blueprint_proposed: true,
        ..Default::default()
    };
    let call = |name: &str| ToolCall {
        id: format!("{name}-call"),
        name: name.to_string(),
        arguments: "{}".to_string(),
    };
    for name in [
        crate::sandbox_link::SANDBOX_WRITE_TOOL,
        crate::sandbox_link::SANDBOX_EXEC_TOOL,
        crate::sandbox_link::SANDBOX_CREATE_TOOL,
        crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL,
        TEMPS_WRITE_TOOL_NAME,
    ] {
        let result = dispatch_conversation_tool(
            &call(name),
            None,
            1,
            "assistant",
            &test_auth(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut state,
        )
        .await;
        assert!(
            result.starts_with("Not run: you proposed an architecture"),
            "{name}: {result}"
        );
        assert!(result.contains(name));
    }
    assert!(may_follow_a_proposal("temps"));
    assert!(may_follow_a_proposal(
        crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL
    ));
    assert!(!may_follow_a_proposal(
        crate::workspace_chats::BIND_BLUEPRINT_TOOL
    ));
}

/// Gateway providers dispatch tool calls straight through
/// `dispatch_conversation_tool`, not through the harness executor, so the
/// incident refusal must live there: a refused tool never runs, never
/// sets turn state, and every attempt is looked up for the timeline.
#[tokio::test]
async fn an_incident_chat_refuses_unattended_tools_at_dispatch() {
    let refused = [
        crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL,
        crate::workspace_chats::BIND_BLUEPRINT_TOOL,
        TEMPS_GOAL_TOOL_NAME,
        crate::secret_slots::REQUEST_SECRET_TOOL,
        crate::ask_user::ASK_USER_TOOL,
        crate::sandbox_link::SANDBOX_OFFER_TOOL,
        crate::sandbox_link::SANDBOX_CREATE_TOOL,
        crate::sandbox_deploy::SANDBOX_DEPLOY_TOOL,
    ];
    let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
    for _ in refused {
        // The incident lookup finds nothing, so recording stops there.
        mock = mock.append_query_results([Vec::<temps_entities::autofix_incidents::Model>::new()]);
    }
    let db = Arc::new(mock.into_connection());
    let mut state = ToolExecutionState {
        incident: Some(IncidentToolGuard {
            public_id: "afx_test".to_string(),
            db: db.clone(),
        }),
        ..Default::default()
    };
    for name in refused {
        let result = dispatch_conversation_tool(
            &ToolCall {
                id: format!("{name}-call"),
                name: name.to_string(),
                arguments: r#"{"title":"x"}"#.to_string(),
            },
            None,
            1,
            "assistant",
            &test_auth(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut state,
        )
        .await;
        assert_eq!(result, crate::autofix::refused_tool_message(name));
    }
    assert!(!state.blueprint_proposed);
    assert!(state.seen_calls.is_empty());
    drop(state);
    let log = Arc::try_unwrap(db)
        .unwrap_or_else(|_| panic!("the test holds the last database handle"))
        .into_transaction_log();
    assert_eq!(log.len(), refused.len(), "each refusal is recorded");
}

/// Tools the incident allows still dispatch normally.
#[tokio::test]
async fn an_incident_chat_still_runs_its_sandbox_and_read_tools() {
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let mut state = ToolExecutionState {
        incident: Some(IncidentToolGuard {
            public_id: "afx_test".to_string(),
            db,
        }),
        ..Default::default()
    };
    let result = dispatch_conversation_tool(
        &ToolCall {
            id: "exec-call".to_string(),
            name: crate::sandbox_link::SANDBOX_EXEC_TOOL.to_string(),
            arguments: "{}".to_string(),
        },
        None,
        1,
        "assistant",
        &test_auth(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        &mut state,
    )
    .await;
    assert_eq!(result, "Sandboxes are not available in this chat.");
}

async fn ask_user(arguments: &str, interactions: Option<&PlatformInteractionExecutor>) -> String {
    dispatch_conversation_tool(
        &ToolCall {
            id: "ask-call".to_string(),
            name: crate::ask_user::ASK_USER_TOOL.to_string(),
            arguments: arguments.to_string(),
        },
        None,
        1,
        "assistant",
        &test_auth(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        interactions,
        &mut ToolExecutionState::default(),
    )
    .await
}

#[tokio::test]
async fn ask_user_shows_a_question_card_and_returns_the_answers() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let interactions = scripted_interactions(
        Some(PermissionDecision::AnswerQuestion {
            answers: serde_json::json!({
                "Which database should I create?": "PostgreSQL",
                "What should it be called?": "orders-db",
            }),
        }),
        seen.clone(),
    );
    let result = ask_user(
        r#"{"questions":[
                {"question":"Which database should I create?","header":"Database",
                 "options":[{"label":"PostgreSQL"},{"label":"Redis"}]},
                {"question":"What should it be called?"}]}"#,
        Some(&interactions),
    )
    .await;

    let shown = seen.lock().expect("seen lock").clone();
    assert_eq!(shown.len(), 1, "exactly one card is shown");
    assert_eq!(shown[0].kind, PermissionKind::Question);
    assert_eq!(shown[0].tool_name, crate::ask_user::ASK_USER_TOOL);
    assert_eq!(
        shown[0].input["questions"][0]["options"][1]["label"],
        "Redis"
    );

    let result: serde_json::Value = serde_json::from_str(&result).expect("json result");
    assert_eq!(result["status"], "answered");
    assert_eq!(result["answers"][0]["answer"], "PostgreSQL");
    assert_eq!(result["answers"][1]["answer"], "orders-db");
}

#[tokio::test]
async fn ask_user_is_never_answered_automatically() {
    let request = PermissionRequest {
        id: "q".to_string(),
        kind: PermissionKind::Question,
        tool_name: crate::ask_user::ASK_USER_TOOL.to_string(),
        input: serde_json::json!({ "questions": [{ "question": "Which?" }] }),
    };
    assert!(automatic_platform_decision(&request).is_none());
}

#[tokio::test]
async fn ask_user_without_an_answer_tells_the_model_not_to_guess() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stopped = scripted_interactions(None, seen.clone());
    let result = ask_user(
        r#"{"questions":[{"question":"Which region?"}]}"#,
        Some(&stopped),
    )
    .await;
    assert!(result.contains("\"not_answered\""), "{result}");

    let dismissed = scripted_interactions(
        Some(PermissionDecision::DenyTool { reason: None }),
        seen.clone(),
    );
    let result = ask_user(
        r#"{"questions":[{"question":"Which zone?"}]}"#,
        Some(&dismissed),
    )
    .await;
    assert!(result.contains("dismissed"), "{result}");

    let result = ask_user(r#"{"questions":[{"question":"Which plan?"}]}"#, None).await;
    assert!(result.contains("\"not_answered\""), "{result}");
}

#[tokio::test]
async fn ask_user_rejects_a_malformed_call_without_showing_a_card() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let interactions = scripted_interactions(None, seen.clone());
    let result = ask_user(r#"{"questions":[]}"#, Some(&interactions)).await;
    assert!(result.contains("1 to 4 questions"), "{result}");
    assert!(seen.lock().expect("seen lock").is_empty());
}

#[test]
fn ask_user_answers_are_kept_in_the_transcript() {
    let result = crate::ask_user::answered_result(&[crate::ask_user::AskAnswer {
        question: "Which database?".to_string(),
        header: None,
        answer: "PostgreSQL".to_string(),
    }]);
    let public = public_tool_result(crate::ask_user::ASK_USER_TOOL, &result);
    assert!(public.contains("PostgreSQL"), "{public}");
    assert!(!public.contains("withheld"), "{public}");
}

// --- service-layer DB tests (MockDatabase) ------------------------------

fn activity_row(
    application_id: Option<i64>,
    provider: &str,
    status: &str,
    count: i64,
) -> std::collections::BTreeMap<String, sea_orm::Value> {
    std::collections::BTreeMap::from([
        ("application_id".to_string(), application_id.into()),
        ("ai_provider".to_string(), provider.to_string().into()),
        ("turn_status".to_string(), status.to_string().into()),
        ("thread_count".to_string(), count.into()),
    ])
}

#[tokio::test]
async fn workspace_activity_counts_returns_grouped_counts_beyond_list_page_size() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![
            activity_row(None, "codex", "running", 31),
            activity_row(Some(11), "claude", "completed", 42),
            activity_row(Some(11), "claude", "failed", 3),
        ]])
        .into_connection();
    let rows = db_service(db)
        .workspace_activity_counts(5, &[], &[11])
        .await
        .expect("grouped workspace counts");
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (
            rows[0].application_id,
            rows[0].ai_provider.as_str(),
            rows[0].turn_status.as_str(),
            rows[0].thread_count
        ),
        (None, "codex", "running", 31)
    );
    assert_eq!(
        (
            rows[1].application_id,
            rows[1].turn_status.as_str(),
            rows[1].thread_count
        ),
        (Some(11), "completed", 42)
    );
    assert_eq!(
        (
            rows[2].application_id,
            rows[2].turn_status.as_str(),
            rows[2].thread_count
        ),
        (Some(11), "failed", 3)
    );
}

#[tokio::test]
async fn workspace_activity_counts_applies_owner_status_and_visibility_in_sql() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());
    assert!(service
        .workspace_activity_counts(5, &[7, 9], &[11, 13])
        .await
        .expect("visible counts")
        .is_empty());
    drop(service);
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("COUNT(*)"));
    assert!(sql.contains("\"created_by\" ="));
    assert!(sql.contains("\"status\" ="));
    assert!(sql.contains("\"application_id\" IN"));
    assert!(sql.contains("\"project_id\" NOT IN"));
    assert!(sql.contains("GROUP BY"));
    assert!(!sql.contains("LIMIT"));
    assert!(!sql.contains("OFFSET"));
}

#[tokio::test]
async fn workspace_activity_counts_with_no_apps_queries_global_only() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());
    assert!(service
        .workspace_activity_counts(5, &[], &[])
        .await
        .expect("global counts")
        .is_empty());
    drop(service);
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("\"context_type\" ="));
    assert!(sql.contains("\"application_id\" IS NULL"));
    assert!(!sql.contains("\"application_id\" IN"));
}

#[tokio::test]
async fn workspace_activity_counts_preserves_database_failure() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("activity unavailable".to_string())])
        .into_connection();
    let error = db_service(db)
        .workspace_activity_counts(5, &[], &[])
        .await
        .expect_err("database failure");
    assert!(
        matches!(error, ChatError::Db(sea_orm::DbErr::Custom(message)) if message == "activity unavailable")
    );
}

/// A `ConversationService` backed by the given mock DB. The AI is a dummy
/// (`ScriptedAi` with no scripted responses) since these tests exercise only
/// the DB query/scoping logic, never an AI turn.
fn db_service(db: DatabaseConnection) -> ConversationService {
    db_service_from_arc(Arc::new(db))
}

fn db_service_from_arc(db: Arc<DatabaseConnection>) -> ConversationService {
    ConversationService {
        db,
        ai: Arc::new(ScriptedAi::new(vec![])),
        providers: HashMap::new(),
        write_support: None,
        goal_support: None,
        secret_support: None,
        config: None,
        application_workspaces: None,
        application_sandboxes: None,
        source_drop_deployer: Default::default(),
        application_service: None,
        blueprint_support: None,
        pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
        harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
    }
}

/// Reports a fixed default chat provider pick (or none).
struct DefaultProviderAi {
    pick: Option<&'static str>,
}

#[async_trait]
impl AiService for DefaultProviderAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn default_chat_provider(&self) -> Option<String> {
        self.pick.map(str::to_string)
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
}

fn active_provider_key(id: i32, provider: &str) -> temps_entities::ai_provider_keys::Model {
    temps_entities::ai_provider_keys::Model {
        id,
        provider: provider.to_string(),
        display_name: format!("{provider} {id}"),
        api_key_encrypted: "encrypted".to_string(),
        base_url: None,
        default_model: None,
        is_active: true,
        trusted_private_address: None,
        trusted_private_address_set_by: None,
        trusted_private_address_set_at: None,
        capabilities: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
async fn default_provider_uses_the_adapters_chat_capable_pick() {
    // Key 1 may be a self-hosted key without streaming; the adapter picked
    // key 2 with one bounded query, so no key list is read here.
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let service = db_service_with_ai(
        db,
        Arc::new(DefaultProviderAi {
            pick: Some("gateway_key:2"),
        }),
    );
    assert_eq!(
        service.resolve_default_provider().await.unwrap(),
        "gateway_key:2"
    );
}

#[tokio::test]
async fn default_provider_keeps_the_first_key_when_none_can_serve_chat() {
    // The readiness check then explains what the key is missing.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![active_provider_key(1, "openai_compatible")]])
        .into_connection();
    let service = db_service_with_ai(db, Arc::new(DefaultProviderAi { pick: None }));
    assert_eq!(
        service.resolve_default_provider().await.unwrap(),
        "gateway_key:1"
    );
}

#[test]
fn no_tool_calling_note_is_added_to_the_system_message() {
    let mut messages = vec![
        ChatMessage::system("You are Temps."),
        ChatMessage {
            role: "user".to_string(),
            content: "redeploy my app".to_string(),
            ..Default::default()
        },
    ];
    append_system_note(&mut messages, NO_TOOL_CALLING_NOTE);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].content.starts_with("You are Temps."));
    assert!(messages[0]
        .content
        .contains("does not support tool calling"));

    let mut without_system = vec![ChatMessage {
        role: "user".to_string(),
        content: "hi".to_string(),
        ..Default::default()
    }];
    append_system_note(&mut without_system, NO_TOOL_CALLING_NOTE);
    assert_eq!(without_system[0].role, "system");
}

fn db_service_with_ai(db: DatabaseConnection, ai: Arc<dyn AiService>) -> ConversationService {
    ConversationService {
        db: Arc::new(db),
        ai,
        providers: HashMap::new(),
        write_support: None,
        goal_support: None,
        secret_support: None,
        config: None,
        application_workspaces: None,
        application_sandboxes: None,
        source_drop_deployer: Default::default(),
        application_service: None,
        blueprint_support: None,
        pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
        harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
    }
}

fn stored_message(id: i64, conversation_id: i64, role: &str) -> ai_messages::Model {
    ai_messages::Model {
        id,
        conversation_id,
        role: role.to_string(),
        content: format!("{role}-{id}"),
        metadata: None,
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn messages_page_returns_latest_page_oldest_first_with_cursor() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                stored_message(8, 42, "assistant"),
                stored_message(7, 42, "user"),
                stored_message(6, 42, "assistant"),
            ]])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    let page = service
        .messages_page(42, None, 2)
        .await
        .expect("latest message page");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![7, 8]
    );
    assert!(page.has_more);
    assert_eq!(page.next_before.as_deref(), Some("m1_7"));
    assert_eq!(
        decode_message_before_cursor(page.next_before.as_deref().expect("page cursor")),
        Ok(7)
    );

    drop(service);
    let db = Arc::try_unwrap(db).expect("release mock database");
    let transaction_log = db.into_transaction_log();
    let statement = &transaction_log[0].statements()[0];
    assert!(statement
        .sql
        .contains("ORDER BY \"ai_messages\".\"id\" DESC"));
    assert!(statement.sql.contains("LIMIT"));
}

#[tokio::test]
async fn diagnostic_messages_are_bounded_and_exclude_internal_rows() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![
            stored_message(3, 42, "assistant"),
            stored_message(2, 42, "user"),
            stored_message(1, 42, "system"),
        ]])
        .into_connection();
    let service = service_with_db(Arc::new(ScriptedAi::new(Vec::new())), db);

    let snapshot = service
        .diagnostic_messages(42, 2)
        .await
        .expect("diagnostic rows should load");

    assert!(!snapshot.truncated);
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(snapshot.messages[0].id, 2);
    assert_eq!(snapshot.messages[1].id, 3);
}

#[tokio::test]
async fn messages_page_before_cursor_is_exclusive_and_pages_earlier_rows() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                stored_message(6, 42, "assistant"),
                stored_message(5, 42, "user"),
                stored_message(4, 42, "assistant"),
            ]])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    let page = service
        .messages_page(42, Some(7), 2)
        .await
        .expect("earlier message page");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert!(page.has_more);
    assert_eq!(page.next_before.as_deref(), Some("m1_5"));

    drop(service);
    let db = Arc::try_unwrap(db).expect("release mock database");
    let transaction_log = db.into_transaction_log();
    let statement = &transaction_log[0].statements()[0];
    assert!(statement.sql.contains("\"ai_messages\".\"id\" <"));
    assert!(format!("{statement:?}").contains('7'));
}

#[tokio::test]
async fn messages_page_omits_cursor_when_no_earlier_rows_remain() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![
            stored_message(2, 42, "assistant"),
            stored_message(1, 42, "user"),
        ]])
        .into_connection();

    let page = db_service(db)
        .messages_page(42, None, 2)
        .await
        .expect("complete message page");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(!page.has_more);
    assert_eq!(page.next_before, None);
}

#[tokio::test]
async fn messages_page_excludes_internal_system_and_summary_rows() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                stored_message(4, 42, "summary"),
                stored_message(3, 42, "assistant"),
                stored_message(2, 42, "system"),
                stored_message(1, 42, "user"),
            ]])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    let page = service
        .messages_page(42, None, 10)
        .await
        .expect("visible message page");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| (message.id, message.role.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "user"), (3, "assistant")]
    );

    drop(service);
    let db = Arc::try_unwrap(db).expect("release mock database");
    let statement = format!("{:?}", db.into_transaction_log()[0].statements()[0]);
    assert!(statement.contains("system"));
    assert!(statement.contains("summary"));
}

#[tokio::test]
async fn provider_aware_availability_accepts_authenticated_host_harness() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service_with_ai(db, Arc::new(HostHarnessOnlyAi));

    assert!(
        !svc.ai_available().await,
        "the default gateway is unavailable"
    );
    assert!(
        svc.ai_available_for(Some("claude_cli")).await,
        "the selected host harness remains available"
    );
    assert!(!svc.ai_available_for(Some("gateway_key:1")).await);
}

#[tokio::test]
async fn claim_turn_persists_one_server_owned_running_turn() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let service = db_service(db);
    let conversation = test_conversation();

    service
        .claim_turn(&conversation, "turn-one")
        .await
        .expect("the idle conversation should be claimed");
}

#[tokio::test]
async fn claim_turn_rejects_an_idempotent_duplicate_without_appending_a_message() {
    let mut running = test_conversation();
    running.turn_status = "running".to_string();
    running.active_turn_id = Some("turn-one".to_string());
    running.last_turn_id = Some("turn-one".to_string());
    running.turn_started_at = Some(Utc::now());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .append_query_results([[running.clone()]])
        .into_connection();

    let error = db_service(db)
        .claim_turn(&running, "turn-one")
        .await
        .expect_err("a retry must not start a second harness turn");

    assert!(matches!(
        error,
        ChatError::DuplicateTurn { turn_id, .. } if turn_id == "turn-one"
    ));
}

#[tokio::test]
async fn cancel_turn_clears_a_durable_claim_before_the_task_is_registered() {
    let mut running = test_conversation();
    running.turn_status = "running".to_string();
    running.active_turn_id = Some("turn-raced-stop".to_string());
    running.last_turn_id = running.active_turn_id.clone();
    running.turn_started_at = Some(Utc::now());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let service = db_service(db);
    let mut events = service.subscribe_conversation(running.id);

    assert!(service
        .cancel_turn(&running)
        .await
        .expect("the persisted claim should be cancellable without an abort handle"));
    let event = events
        .try_recv()
        .expect("observers should receive the terminal state");
    assert_eq!(event.event, "turn_complete");
}

#[tokio::test]
async fn cancelled_claim_never_starts_provider_execution_after_registration() {
    let mut cancelled = test_conversation();
    cancelled.turn_status = "cancelled".to_string();
    cancelled.active_turn_id = None;
    cancelled.last_turn_id = Some("turn-cancelled-during-setup".to_string());
    cancelled.turn_started_at = None;
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[cancelled]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![ChatStreamDelta::Text(
        "must not run".to_string(),
    )])]));
    let calls = ai.chat_calls.clone();
    let service = db_service_with_ai(db, ai);
    let conversation = test_conversation();

    let stream = service
        .try_tool_loop_in_workspace(
            &conversation,
            vec![],
            None,
            vec![],
            &test_auth(),
            &test_request_metadata(),
            None,
            Vec::new(),
            None,
            temps_ai::SensitiveEnvironment::default(),
            Vec::new(),
            Some("turn-cancelled-during-setup".to_string()),
            false,
            None,
        )
        .await;
    let output = drain(stream).await;

    assert!(output.is_empty());
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a task whose durable claim was cancelled must not reach the provider"
    );
}

#[tokio::test]
async fn project_chat_preserves_reasoning_supported_by_responses_api() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let service = db_service(db);

    let runtime = service
        .resolve_conversation_runtime(
            1,
            Some("gateway_key:1"),
            Some("gpt-5.6-luna"),
            Some("medium"),
            Some("confirm-actions"),
        )
        .await
        .expect("Responses API supports reasoning with function tools");

    assert_eq!(runtime.thinking_level.as_deref(), Some("medium"));
}

#[tokio::test]
async fn update_runtime_options_changes_model_without_switching_provider_and_resets_session() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "codex_cli".to_string();
    conversation.cli_session_id = Some("session-1".to_string());
    conversation.cli_session_fingerprint = Some("v1:fingerprint".to_string());

    let mut updated = conversation.clone();
    updated.ai_model = "gpt-4.1".to_string();
    updated.ai_thinking_level = Some("low".to_string());
    updated.ai_permission_mode = "full-access".to_string();
    updated.cli_session_id = None;
    updated.cli_session_fingerprint = None;
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[updated]])
        .into_connection();

    let result = db_service(db)
        .update_runtime_options(
            &conversation,
            Some("gpt-4.1"),
            Some("low"),
            Some("full-access"),
        )
        .await
        .expect("valid runtime options should persist");

    assert_eq!(result.ai_provider, "codex_cli");
    assert_eq!(result.ai_model, "gpt-4.1");
    assert_eq!(result.ai_thinking_level.as_deref(), Some("low"));
    assert_eq!(result.ai_permission_mode, "full-access");
    assert_eq!(result.cli_session_id, None);
    assert_eq!(result.cli_session_fingerprint, None);
}

#[tokio::test]
async fn update_runtime_options_retains_session_when_only_thinking_and_permission_change() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "codex_cli".to_string();
    conversation.cli_session_id = Some("session-1".to_string());
    conversation.cli_session_fingerprint = Some("v1:fingerprint".to_string());

    let mut updated = conversation.clone();
    updated.ai_thinking_level = Some("high".to_string());
    updated.ai_permission_mode = "full-access".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[updated]])
        .into_connection();

    let result = db_service(db)
        .update_runtime_options(&conversation, None, Some("high"), Some("full-access"))
        .await
        .expect("turn-level options should persist");

    assert_eq!(result.ai_provider, "codex_cli");
    assert_eq!(result.ai_model, conversation.ai_model);
    assert_eq!(result.cli_session_id.as_deref(), Some("session-1"));
    assert_eq!(
        result.cli_session_fingerprint.as_deref(),
        Some("v1:fingerprint")
    );
}

#[tokio::test]
async fn update_runtime_selection_moves_a_gateway_chat_to_another_gateway_key() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "gateway_key:1".to_string();
    conversation.ai_model = "gpt-4o-mini".to_string();
    conversation.cli_session_id = Some("session-1".to_string());
    conversation.cli_session_fingerprint = Some("v1:fingerprint".to_string());

    let mut updated = conversation.clone();
    updated.ai_provider = "gateway_key:2".to_string();
    updated.cli_session_id = None;
    updated.cli_session_fingerprint = None;
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[updated]])
            .into_connection(),
    );

    // Same model id on the new key: the provider change alone must still
    // persist and start a fresh provider session.
    let result = db_service_from_arc(db.clone())
        .update_runtime_selection(
            &conversation,
            Some("gateway_key:2"),
            Some("gpt-4o-mini"),
            None,
            None,
        )
        .await
        .expect("a gateway chat may move to another gateway key");
    assert_eq!(result.ai_provider, "gateway_key:2");

    let db = Arc::try_unwrap(db).expect("service dropped its handle");
    let log = db.into_transaction_log();
    let statements = log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .collect::<Vec<_>>();
    assert_eq!(statements.len(), 1);
    assert!(statements[0].sql.contains("UPDATE \"ai_conversations\""));
    assert!(statements[0].sql.contains("\"ai_provider\""));
    let statement = format!("{:?}", statements[0]);
    assert!(statement.contains("gateway_key:2"));
    assert!(!statement.contains("session-1"));
    assert!(!statement.contains("v1:fingerprint"));
}

#[tokio::test]
async fn update_runtime_selection_keeps_a_harness_chat_on_its_provider() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "codex_cli".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let error = db_service(db)
        .update_runtime_selection(&conversation, Some("gateway_key:2"), None, None, None)
        .await
        .expect_err("a harness chat must not move to another provider");

    assert!(matches!(error, ChatError::Ai(message) if
        message.contains("codex_cli") && message.contains("gateway_key:2")));
}

#[tokio::test]
async fn update_runtime_selection_does_not_move_a_gateway_chat_onto_a_harness() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "gateway_key:1".to_string();
    conversation.ai_model = "gpt-4o-mini".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let error = db_service(db)
        .update_runtime_selection(&conversation, Some("codex_cli"), None, None, None)
        .await
        .expect_err("a gateway chat must not move onto a host harness");

    assert!(matches!(error, ChatError::Ai(message) if
        message.contains("gateway_key:1") && message.contains("codex_cli")));
}

#[tokio::test]
async fn update_runtime_options_rejects_options_outside_pinned_provider_capabilities() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "codex_cli".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let error = db_service(db)
        .update_runtime_options(&conversation, Some("claude-opus"), None, None)
        .await
        .expect_err("a model outside the pinned harness must fail closed");

    assert!(matches!(error, ChatError::Ai(message) if
        message.contains("claude-opus") && message.contains("codex_cli")));
}

#[tokio::test]
async fn update_runtime_options_revalidates_an_unchanged_persisted_model() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_model = "claude-model-no-longer-available".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

    let error = db_service(db)
        .update_runtime_options(&conversation, None, None, None)
        .await
        .expect_err("a stale persisted model must not bypass capability validation");

    assert!(matches!(error, ChatError::Ai(message) if
        message.contains("claude-model-no-longer-available")
            && message.contains("claude_cli")));
}

#[tokio::test]
async fn update_runtime_options_persists_reconciled_tool_thinking_mode() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "claude_cli".to_string();
    conversation.ai_thinking_level = Some("high".to_string());
    let mut updated = conversation.clone();
    updated.ai_thinking_level = Some("medium".to_string());
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[updated]])
        .into_connection();

    let result = db_service_with_ai(db, Arc::new(ToolRestrictedCapabilitiesAi))
        .update_runtime_options(&conversation, None, None, None)
        .await
        .expect("the reconciled tool-safe thinking mode should persist");

    assert_eq!(result.ai_thinking_level.as_deref(), Some("medium"));
}

#[tokio::test]
async fn update_runtime_options_does_not_reject_from_non_authoritative_catalogs() {
    for source in [
        temps_ai::ModelCatalogSource::Bootstrap,
        temps_ai::ModelCatalogSource::StaleCache,
    ] {
        for model in ["workspace-entitled-model", "gpt-4o-mini"] {
            for thinking_level in [Some("medium".to_string()), None] {
                let mut conversation = test_conversation();
                conversation.ai_provider = "claude_cli".to_string();
                conversation.ai_model = model.to_string();
                conversation.ai_thinking_level = thinking_level;
                let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
                let result =
                    db_service_with_ai(db, Arc::new(NonAuthoritativeCapabilitiesAi(source)))
                        .update_runtime_options(&conversation, None, None, None)
                        .await
                        .expect("fallback catalogs are not account allowlists");

                assert_eq!(result, conversation);
            }
        }
    }
}

#[tokio::test]
async fn update_runtime_options_preserves_database_failure() {
    let mut conversation = test_conversation();
    conversation.ai_provider = "codex_cli".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("write failed".to_string())])
        .into_connection();

    let error = db_service(db)
        .update_runtime_options(&conversation, None, Some("high"), None)
        .await
        .expect_err("database failures must not look like successful updates");

    assert!(
        matches!(error, ChatError::Db(sea_orm::DbErr::Custom(message)) if
        message == "write failed")
    );
}

/// Build a conversation row for a given project, with controllable public_id.
fn conv_for(id: i64, project_id: i32, public_id: &str) -> ai_conversations::Model {
    let now = Utc::now();
    ai_conversations::Model {
        id,
        public_id: public_id.to_string(),
        project_id: Some(project_id),
        application_id: None,
        context_type: "deployment".to_string(),
        context_id: "1".to_string(),
        title: Some("t".to_string()),
        status: "active".to_string(),
        kind: "ai".to_string(),
        created_by: 5,
        metadata: None,
        cli_session_id: None,
        cli_session_fingerprint: None,
        ai_provider: "gateway".to_string(),
        ai_model: "gpt-4o-mini".to_string(),
        ai_thinking_level: None,
        ai_permission_mode: "confirm-actions".to_string(),
        turn_status: "idle".to_string(),
        active_turn_id: None,
        last_turn_id: None,
        turn_started_at: None,
        next_step_suggestions: None,
        created_at: now,
        last_activity_at: now,
    }
}

fn system_message(id: i64, conversation_id: i64) -> ai_messages::Model {
    ai_messages::Model {
        id,
        conversation_id,
        role: "system".to_string(),
        content: "private system context".to_string(),
        metadata: None,
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: Utc::now(),
    }
}

/// Build a minimal valid legacy project row. Toggle values are varied to
/// prove they no longer influence current chat authorization.
fn project_with_toggle(
    id: i32,
    name: &str,
    slug: &str,
    toggle: Option<bool>,
) -> temps_entities::projects::Model {
    let now = Utc::now();
    temps_entities::projects::Model {
        id,
        image_retention_hours: None,
        log_ingest_lines_per_sec: None,
        log_ingest_kib_per_sec: None,
        cloud_telemetry_fidelity:
            temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity::Metered,
        cloud_telemetry_write_mode:
            temps_entities::cloud_telemetry_write_mode::CloudTelemetryWriteMode::Local,
        cloud_analytics_write_mode:
            temps_entities::cloud_analytics_write_mode::CloudAnalyticsWriteMode::Local,
        instantiation_token: None,
        cloud_telemetry_attribute_allowlist: Vec::new(),
        name: name.to_string(),
        repo_name: "r".to_string(),
        repo_owner: "o".to_string(),
        directory: ".".to_string(),
        pull_only_root_directory: false,
        main_branch: "main".to_string(),
        preset: temps_entities::preset::Preset::Static,
        preset_config: None,
        deployment_config: None,
        created_at: now,
        updated_at: now,
        slug: slug.to_string(),
        template_slug: None,
        is_deleted: false,
        deleted_at: None,
        last_deployment: None,
        is_public_repo: false,
        git_url: None,
        git_provider_connection_id: None,
        attack_mode: false,
        shield_mode: "log".to_string(),
        ai_alert_summaries_enabled: None,
        ai_api_traffic_summary_enabled: None,
        allow_alternate_sources: None,
        hosting: Default::default(),
        ai_debug_chat_enabled: toggle,
        ai_write_actions_enabled: false,
        error_source_context_enabled: false,
        vulnerability_scanning_enabled: false,
        error_source_root: None,
        enable_preview_environments: false,
        preview_envs_on_demand: false,
        preview_envs_idle_timeout_seconds: 300,
        preview_envs_wake_timeout_seconds: 30,
        source_type: temps_entities::source_type::SourceType::Git,
        project_type: temps_entities::types::ProjectType::Server,
        service_template: None,
        gitlab_webhook_id: None,
        gitlab_webhook_signing_token: None,
        gitea_webhook_signing_token: None,
        github_webhook_id: None,
        github_webhook_signing_token: None,
        bitbucket_webhook_token: None,
        bitbucket_webhook_hook_id: None,
        generic_webhook_token: None,
        cross_project_trace_sharing: true,
        serves_own_temps_api: false,
    }
}

#[tokio::test]
async fn two_users_get_separate_conversations_for_the_same_context() {
    let mut user_11 = conv_for(1, 7, "user11");
    user_11.created_by = 11;
    user_11.ai_provider = "gateway_key:1".to_string();
    let mut user_22 = conv_for(2, 7, "user22");
    user_22.created_by = 22;
    user_22.ai_provider = "gateway_key:1".to_string();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<ai_conversations::Model>::new()])
        .append_query_results([vec![user_11.clone()]])
        .append_query_results([vec![system_message(1, user_11.id)]])
        .append_query_results([Vec::<ai_conversations::Model>::new()])
        .append_query_results([vec![user_22.clone()]])
        .append_query_results([vec![system_message(2, user_22.id)]])
        .into_connection();
    let mut providers: HashMap<&'static str, Arc<dyn ConversationContextProvider>> = HashMap::new();
    providers.insert("deployment", Arc::new(SeedOnlyProvider));
    let svc = ConversationService {
        db: Arc::new(db),
        ai: Arc::new(ScriptedAi::new(vec![])),
        providers,
        write_support: None,
        goal_support: None,
        secret_support: None,
        config: None,
        application_workspaces: None,
        application_sandboxes: None,
        source_drop_deployer: Default::default(),
        application_service: None,
        blueprint_support: None,
        pending_permissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        conversation_broadcasts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        active_turns: Arc::new(std::sync::Mutex::new(HashMap::new())),
        harness_mcp_entries: Arc::new(std::sync::Mutex::new(HashMap::new())),
    };

    let first = svc
        .get_or_create(
            Some(7),
            "deployment",
            "1",
            11,
            Some("gateway_key:1"),
            Some("gpt-4o-mini"),
            None,
            Some("confirm-actions"),
        )
        .await
        .expect("first user's private conversation");
    let second = svc
        .get_or_create(
            Some(7),
            "deployment",
            "1",
            22,
            Some("gateway_key:1"),
            Some("gpt-4o-mini"),
            None,
            Some("confirm-actions"),
        )
        .await
        .expect("second user's private conversation");

    assert_eq!(first.created_by, 11);
    assert_eq!(second.created_by, 22);
    assert_ne!(first.public_id, second.public_id);
}

#[tokio::test]
async fn chat_readiness_ignores_legacy_project_chat_opt_out() {
    let project = project_with_toggle(7, "Alpha", "alpha", Some(false));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![project]])
        .into_connection();

    let readiness = db_service(db).chat_readiness(7).await.expect("readiness");
    assert_eq!(
        readiness,
        ChatReadiness {
            ai_configured: true,
        }
    );
}

#[tokio::test]
async fn chat_readiness_reports_unconfigured_ai() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![project_with_toggle(
            7,
            "Alpha",
            "alpha",
            Some(true),
        )]])
        .into_connection();
    let svc = db_service_with_ai(db, Arc::new(ScriptedAi::unavailable()));

    let readiness = svc.chat_readiness(7).await.expect("readiness");
    assert!(!readiness.ai_configured);
}

#[tokio::test]
async fn chat_readiness_returns_typed_not_found() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
        .into_connection();

    let err = db_service(db)
        .chat_readiness(404)
        .await
        .expect_err("missing project");
    assert!(matches!(err, ChatError::ProjectNotFound(404)));
}

#[tokio::test]
async fn chat_readiness_preserves_project_context_on_database_failure() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("connection lost".to_string())])
        .into_connection();

    let err = db_service(db)
        .chat_readiness(42)
        .await
        .expect_err("query failure");
    assert!(matches!(
        err,
        ChatError::ProjectLookup { project_id: 42, .. }
    ));
}

// find_by_context: returns the active conversation when one exists.
#[tokio::test]
async fn test_find_by_context_returns_match() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
        .into_connection();
    let svc = db_service(db);

    let found = svc
        .find_by_context(Some(7), 5, "deployment", "1")
        .await
        .expect("query ok");
    let conv = found.expect("a conversation should be found");
    assert_eq!(conv.project_id, Some(7));
    assert_eq!(conv.public_id, "pubA");
}

// find_by_context: returns None when no row matches.
#[tokio::test]
async fn test_find_by_context_none_when_absent() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
        .into_connection();
    let svc = db_service(db);

    let found = svc
        .find_by_context(Some(7), 5, "deployment", "1")
        .await
        .expect("query ok");
    assert!(found.is_none());
}

#[tokio::test]
async fn every_conversation_lookup_is_scoped_to_the_current_creator() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let svc = db_service_from_arc(db.clone());

    assert!(svc
        .find_by_context(Some(7), 11, "deployment", "1")
        .await
        .expect("context lookup")
        .is_none());
    assert!(svc
        .list_conversations(7, 11)
        .await
        .expect("project list")
        .is_empty());
    assert!(svc
        .list_all_conversations(11, &[])
        .await
        .expect("global list")
        .is_empty());
    assert!(matches!(
        svc.get_by_public_id(7, 11, "owned-session").await,
        Err(ChatError::NotFound(_))
    ));
    assert!(matches!(
        svc.get_by_id(7, 11, 99).await,
        Err(ChatError::NotFound(_))
    ));

    drop(svc);
    let db = Arc::try_unwrap(db).expect("release mock database");
    let log = db.into_transaction_log();
    let statements = log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .collect::<Vec<_>>();
    assert_eq!(statements.len(), 5);
    assert!(
        statements.iter().all(|statement| {
            statement.sql.contains("created_by") && format!("{statement:?}").contains("11")
        }),
        "every query must bind created_by = current user; got {statements:?}"
    );
}

#[tokio::test]
async fn another_project_member_cannot_load_or_resume_an_owned_cli_session() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let svc = db_service_from_arc(db.clone());

    let error = svc
        .get_by_public_id(7, 22, "user-11-claude-session")
        .await
        .expect_err("another member must receive the same not-found as an unknown id");
    assert!(matches!(error, ChatError::NotFound(id) if id == "user-11-claude-session"));

    drop(svc);
    let db = Arc::try_unwrap(db).expect("release mock database");
    let dump = format!("{:?}", db.into_transaction_log());
    assert!(dump.contains("created_by") && dump.contains("22"));
}

#[tokio::test]
async fn test_get_by_id_retains_project_scope() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
        .into_connection();
    let conv = db_service(db)
        .get_by_id(7, 5, 1)
        .await
        .expect("conversation");
    assert_eq!(conv.public_id, "pubA");

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
        .into_connection();
    let err = db_service(db)
        .get_by_id(8, 5, 1)
        .await
        .expect_err("cross-project child lookup must not resolve");
    assert!(matches!(err, ChatError::NotFound(_)));
}

// list_conversations: returns the project's active conversations.
#[tokio::test]
async fn test_list_conversations_returns_rows() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![conv_for(1, 7, "pubA"), conv_for(2, 7, "pubB")]])
        .into_connection();
    let svc = db_service(db);

    let convs = svc.list_conversations(7, 5).await.expect("query ok");
    assert_eq!(convs.len(), 2);
    assert!(convs.iter().all(|c| c.project_id == Some(7)));
}

// list_all_conversations: annotates each conversation with its project's
// name/slug, scoped to the current creator.
#[tokio::test]
async fn test_list_all_conversations_annotates_enabled_projects() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // 1st query: the conversations.
        .append_query_results(vec![vec![conv_for(1, 7, "pubA"), conv_for(2, 8, "pubB")]])
        // 2nd query: the projects for those ids (both enabled).
        .append_query_results(vec![vec![
            project_with_toggle(7, "Alpha", "alpha", Some(true)),
            project_with_toggle(8, "Beta", "beta", Some(true)),
        ]])
        .into_connection();
    let svc = db_service(db);

    let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
    assert_eq!(items.len(), 2);
    let alpha = items
        .iter()
        .find(|i| i.conversation.project_id == Some(7))
        .expect("alpha present");
    assert_eq!(alpha.project_name.as_deref(), Some("Alpha"));
    assert_eq!(alpha.project_slug.as_deref(), Some("alpha"));
}

/// Chats in one workspace link the same sandbox, and every one of them
/// shows it in the list, not only the first.
#[tokio::test]
async fn every_chat_sharing_a_sandbox_lists_it() {
    let link = serde_json::json!({
        "sandbox_link": {
            "sandbox_id": "sbx_shared",
            "name": "shared",
            "linked_at": "2026-01-01T00:00:00Z"
        }
    });
    let chat = |id: i64, public_id: &str| ai_conversations::Model {
        project_id: None,
        context_type: crate::providers::assistant::ASSISTANT_CONTEXT_TYPE.to_string(),
        metadata: Some(link.clone()),
        ..conv_for(id, 0, public_id)
    };
    let now = Utc::now();
    let sandbox = temps_entities::sandboxes::Model {
        id: 3,
        public_id: "sbx_shared".to_string(),
        user_id: Some(5),
        agent_run_id: None,
        name: "shared".to_string(),
        status: "running".to_string(),
        lifecycle: "workspace".to_string(),
        project_id: None,
        source_repo_url: None,
        image: None,
        work_dir: "/workspace".to_string(),
        timeout_secs: 3600,
        metadata: None,
        backend: Some("docker".to_string()),
        node_id: None,
        created_at: now,
        last_activity_at: now,
        expires_at: now,
        preview_password_hash: None,
        preview_password_hint: None,
        preview_access: Default::default(),
        preview_allowed_cidrs: Vec::new(),
        egress_policy: Default::default(),
    };
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![chat(1, "first"), chat(2, "second")]])
            .append_query_results([vec![sandbox]])
            .into_connection(),
    );

    let items = db_service_from_arc(db)
        .list_all_conversations(5, &[])
        .await
        .expect("conversation list");

    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(
            item.sandbox.as_ref().map(|s| s.sandbox_id.as_str()),
            Some("sbx_shared"),
            "{}",
            item.conversation.public_id
        );
    }
}

#[tokio::test]
async fn test_list_all_conversations_excludes_currently_hidden_projects() {
    // MockDatabase does not execute query predicates, so return the row the
    // database would retain and separately assert the visibility filter.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![conv_for(2, 8, "visible")]])
            .append_query_results([vec![project_with_toggle(
                8,
                "Visible",
                "visible",
                Some(true),
            )]])
            .into_connection(),
    );

    let items = db_service_from_arc(db.clone())
        .list_all_conversations(5, &[7])
        .await
        .expect("hidden projects filter");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].conversation.public_id, "visible");
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("\"project_id\" NOT IN"));
}

#[tokio::test]
async fn global_workspace_filter_is_applied_before_the_bounded_query() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    assert!(service
        .list_all_conversations_filtered(5, &[], "active", true, 2, 25)
        .await
        .expect("global conversation list")
        .is_empty());

    drop(service);
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("project_id") && sql.contains("IS NULL"));
    assert!(sql.contains("application_id") && sql.contains("IS NULL"));
    assert!(sql.contains("context_type"));
    assert!(sql.contains("LIMIT") && sql.contains("OFFSET"));
}

#[tokio::test]
async fn conversation_visibility_is_applied_in_sql_before_pagination() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    assert!(service
        .list_all_conversations_with_visibility(
            5,
            &[7],
            "active",
            false,
            2,
            25,
            Some(&[11, 13]),
            &["deployment"],
            None,
        )
        .await
        .expect("visibility-filtered conversation page")
        .is_empty());

    drop(service);
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("\"project_id\" NOT IN"));
    assert!(sql.contains("\"project_id\" IN (SELECT \"id\" FROM \"projects\")"));
    assert!(sql.contains("\"context_type\" <>"));
    assert!(sql.contains("\"application_id\" IN"));
    assert!(sql.contains("\"context_type\" NOT IN"));
    assert!(sql.contains("LIMIT") && sql.contains("OFFSET"));
}

#[tokio::test]
async fn empty_application_visibility_excludes_application_conversations_before_pagination() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    assert!(service
        .list_all_conversations_with_visibility(
            5,
            &[],
            "active",
            false,
            1,
            25,
            Some(&[]),
            &[],
            None,
        )
        .await
        .expect("non-application conversation page")
        .is_empty());

    drop(service);
    let sql = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log()
        .iter()
        .flat_map(|transaction| transaction.statements())
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("\"context_type\" <>"));
    assert!(!sql.contains("\"application_id\" IN"));
    assert!(sql.contains("LIMIT") && sql.contains("OFFSET"));
}

/// Searching must filter in SQL before `OFFSET/LIMIT`; filtering a loaded
/// page would miss every match outside it, which is the bug this fixes
/// (the chat switcher only found chats already in the sidebar).
#[tokio::test]
async fn conversation_search_matches_title_or_project_before_pagination() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ai_conversations::Model>::new()])
            .into_connection(),
    );
    let service = db_service_from_arc(db.clone());

    assert!(service
        .list_all_conversations_with_visibility(
            5,
            &[],
            "active",
            false,
            1,
            25,
            None,
            &[],
            Some("  Barber_50%  "),
        )
        .await
        .expect("searched conversation page")
        .is_empty());

    drop(service);
    let log = Arc::try_unwrap(db)
        .expect("release mock database")
        .into_transaction_log();
    let statements: Vec<_> = log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .collect();
    let sql = statements
        .iter()
        .map(|statement| statement.sql.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sql.contains("LOWER(\"title\") LIKE"), "{sql}");
    assert!(sql.contains("LOWER(\"name\") LIKE"), "{sql}");
    assert!(sql.contains("LOWER(\"slug\") LIKE"), "{sql}");
    assert!(sql.contains("LIMIT") && sql.contains("OFFSET"));
    let values = format!("{:?}", statements[0].values);
    assert!(values.contains(r"%barber\\_50\\%%"), "{values}");
}

#[test]
fn escape_like_matches_metacharacters_literally() {
    assert_eq!(escape_like(r"50%_off\"), r"50\%\_off\\");
    assert_eq!(escape_like("plain"), "plain");
}

// Legacy project toggle values no longer affect chat visibility. Access is
// determined by the current user's project membership and permissions.
#[tokio::test]
async fn test_list_all_conversations_ignores_legacy_project_toggle() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![
            conv_for(1, 7, "pubEnabled"),
            conv_for(2, 8, "pubDisabled"),
            conv_for(3, 9, "pubNull"),
        ]])
        .append_query_results(vec![vec![
            project_with_toggle(7, "Alpha", "alpha", Some(true)),
            project_with_toggle(8, "Beta", "beta", Some(false)),
            project_with_toggle(9, "Gamma", "gamma", None),
        ]])
        .into_connection();
    let svc = db_service(db);

    let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
    assert_eq!(
        items.len(),
        3,
        "all accessible project conversations stay visible"
    );
    assert!(items
        .iter()
        .any(|i| i.conversation.public_id == "pubDisabled"));
    assert!(items
        .iter()
        .any(|i| i.conversation.public_id == "pubEnabled"));
    assert!(items.iter().any(|i| i.conversation.public_id == "pubNull"));
}

// A project deleted after the bounded conversation query must not shrink
// the page during the metadata enrichment query.
#[tokio::test]
async fn test_list_all_conversations_preserves_page_when_project_metadata_disappears() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
        // Project lookup returns nothing for id 7.
        .append_query_results(vec![Vec::<temps_entities::projects::Model>::new()])
        .into_connection();
    let svc = db_service(db);

    let items = svc.list_all_conversations(5, &[]).await.expect("query ok");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].conversation.public_id, "pubA");
    assert_eq!(items[0].project_name, None);
    assert_eq!(items[0].project_slug, None);
}

// get_by_public_id: returns the row when the (project_id, public_id) pair
// matches; the filter scopes to the project so a wrong project can't fetch it.
#[tokio::test]
async fn test_get_by_public_id_returns_scoped_row() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![conv_for(1, 7, "pubA")]])
        .into_connection();
    let svc = db_service(db);

    let conv = svc.get_by_public_id(7, 5, "pubA").await.expect("found");
    assert_eq!(conv.project_id, Some(7));
    assert_eq!(conv.public_id, "pubA");
}

// get_by_public_id: when the scoped query returns no row (e.g. wrong project
// or unknown id), a `NotFound` carrying the public_id is returned.
#[tokio::test]
async fn test_get_by_public_id_not_found_is_scoped_error() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![Vec::<ai_conversations::Model>::new()])
        .into_connection();
    let svc = db_service(db);

    let err = svc
        .get_by_public_id(99, 5, "pubA")
        .await
        .expect_err("should not find a conversation in the wrong project");
    match err {
        ChatError::NotFound(id) => assert_eq!(id, "pubA"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// archive: flips status to "archived" via an UPDATE returning the row.
#[tokio::test]
async fn test_archive_succeeds() {
    let mut archived = conv_for(1, 7, "pubA");
    archived.status = "archived".to_string();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![archived]])
        .into_connection();
    let svc = db_service(db);

    let conv = conv_for(1, 7, "pubA");
    svc.archive(&conv).await.expect("archive ok");
}

#[tokio::test]
async fn test_archive_rejects_a_running_conversation() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let mut running = conv_for(1, 7, "pubA");
    running.turn_status = "running".to_string();

    let error = svc
        .archive(&running)
        .await
        .expect_err("a running conversation must remain visible and cancellable");

    assert!(matches!(
        error,
        ChatError::TurnInProgress { conversation_id } if conversation_id == "pubA"
    ));
}

#[tokio::test]
async fn test_restore_succeeds() {
    let active = conv_for(1, 7, "pubA");
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![active]])
        .into_connection();
    let svc = db_service(db);

    let mut archived = conv_for(1, 7, "pubA");
    archived.status = "archived".to_string();
    svc.restore(&archived).await.expect("restore ok");
}

// ----- Pending-permission registry tests (ADR-038 Phase 2, milestone 3) -----

/// A freshly constructed `ConversationService` has an empty registry.
#[tokio::test]
async fn test_registry_starts_empty() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let registry = svc.pending_permissions.lock().unwrap();
    assert!(registry.is_empty());
}

/// Insert and immediately resolve: sender receives the decision.
#[tokio::test]
async fn test_registry_insert_and_resolve() {
    use temps_ai::streaming::{PermissionDecision, PermissionKind};
    use tokio::sync::oneshot;

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);

    let (tx, rx) = oneshot::channel::<PermissionResolution>();
    {
        let mut registry = svc.pending_permissions.lock().unwrap();
        registry.insert(
            "req-1".to_string(),
            PendingPermissionEntry {
                sender: tx,
                conv_public_id: "pub1".to_string(),
                kind: PermissionKind::ToolApproval,
                tool_name: "Bash".to_string(),
                input: serde_json::Value::Null,
                generation: uuid::Uuid::new_v4(),
                origin: PendingPermissionOrigin::Provider,
            },
        );
    }

    // Resolve via remove-to-claim.
    let entry = {
        let mut registry = svc.pending_permissions.lock().unwrap();
        registry.remove("req-1")
    };

    assert!(entry.is_some(), "entry must be present after insert");
    let entry = entry.unwrap();
    assert_eq!(entry.conv_public_id, "pub1");
    assert_eq!(entry.kind, PermissionKind::ToolApproval);
    let sent = entry.sender.send(PermissionResolution {
        decision: PermissionDecision::AllowTool,
        auth: test_auth(),
        metadata: test_request_metadata(),
    });
    assert!(sent.is_ok(), "the waiting turn must receive the resolution");
    let resolution = rx.await.unwrap();
    assert!(matches!(resolution.decision, PermissionDecision::AllowTool));
}

#[tokio::test]
async fn active_auto_mode_resolves_safe_tools_but_not_destructive_writes() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let running = tokio::spawn(std::future::pending::<()>());
    let auto = Arc::new(AtomicBool::new(false));
    svc.active_turns.lock().unwrap().insert(
        7,
        ActiveTurn {
            turn_id: "turn-7".to_string(),
            abort: running.abort_handle(),
            auto_approve_provider_tools: auto.clone(),
            permission_mode: Arc::new(Mutex::new("default".to_string())),
        },
    );

    let (provider_tx, provider_rx) = oneshot::channel();
    let (platform_tx, platform_rx) = oneshot::channel();
    let (destructive_tx, _destructive_rx) = oneshot::channel();
    let (question_tx, _question_rx) = oneshot::channel();
    {
        let mut registry = svc.pending_permissions.lock().unwrap();
        for (id, sender, kind, origin) in [
            (
                "provider-tool",
                provider_tx,
                PermissionKind::ToolApproval,
                PendingPermissionOrigin::Provider,
            ),
            (
                "platform-tool",
                platform_tx,
                PermissionKind::ToolApproval,
                PendingPermissionOrigin::PlatformWrite,
            ),
            (
                "destructive-platform-tool",
                destructive_tx,
                PermissionKind::ToolApproval,
                PendingPermissionOrigin::PlatformWrite,
            ),
            (
                "provider-question",
                question_tx,
                PermissionKind::Question,
                PendingPermissionOrigin::Provider,
            ),
        ] {
            registry.insert(
                id.to_string(),
                PendingPermissionEntry {
                    sender,
                    conv_public_id: "conversation-7".to_string(),
                    kind,
                    tool_name: "test".to_string(),
                    input: if id == "destructive-platform-tool" {
                        serde_json::json!({"method": "DELETE"})
                    } else {
                        serde_json::Value::Null
                    },
                    generation: uuid::Uuid::new_v4(),
                    origin,
                },
            );
        }
    }

    let update = svc.apply_active_permission_mode(
        7,
        "conversation-7",
        "full-access",
        &test_auth(),
        &test_request_metadata(),
    );

    assert!(update.applied_to_active_turn);
    assert_eq!(update.auto_approved.len(), 2);
    let mut approved_ids = update
        .auto_approved
        .iter()
        .map(|approval| approval.id.as_str())
        .collect::<Vec<_>>();
    approved_ids.sort_unstable();
    assert_eq!(approved_ids, ["platform-tool", "provider-tool"]);
    assert!(update
        .auto_approved
        .iter()
        .all(|approval| approval.delivered));
    assert!(auto.load(Ordering::Acquire));
    assert_eq!(
        active_permission_mode(
            &svc.active_turns
                .lock()
                .unwrap()
                .get(&7)
                .expect("active turn")
                .permission_mode
        ),
        "full-access"
    );
    assert!(matches!(
        provider_rx.await.unwrap().decision,
        PermissionDecision::AllowTool
    ));
    assert!(matches!(
        platform_rx.await.unwrap().decision,
        PermissionDecision::AllowTool
    ));
    let registry = svc.pending_permissions.lock().unwrap();
    assert!(!registry.contains_key("platform-tool"));
    assert!(registry.contains_key("destructive-platform-tool"));
    assert!(registry.contains_key("provider-question"));
    drop(registry);
    running.abort();
}

#[test]
fn assistant_defaults_to_running_changes_but_still_asks_for_deletions() {
    assert!(permission_mode_auto_approves_provider_tools(
        ASSISTANT_DEFAULT_PERMISSION_MODE
    ));
    assert!(!permission_mode_auto_approves_provider_tools(
        "confirm-actions"
    ));
    let delete = PermissionRequest {
        id: "delete".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({"method": "DELETE"}),
    };
    assert!(automatic_platform_decision(&delete).is_none());
}

#[test]
fn platform_auto_approval_never_includes_delete_operations() {
    let safe = PermissionRequest {
        id: "safe".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({"method": "POST"}),
    };
    assert!(matches!(
        automatic_platform_decision(&safe),
        Some(PermissionDecision::AllowTool)
    ));

    let destructive = PermissionRequest {
        id: "delete".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({"method": "DELETE"}),
    };
    assert!(automatic_platform_decision(&destructive).is_none());

    let destructive_plan = PermissionRequest {
        id: "plan".to_string(),
        kind: PermissionKind::PlanApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({
            "steps": [
                {"method": "PATCH"},
                {"method": "delete"}
            ]
        }),
    };
    assert!(automatic_platform_decision(&destructive_plan).is_none());
}

/// A write that spends a credential the user entered privately is never
/// approved by a permission mode on its own: the model picks the other fields
/// (an endpoint, a base URL) and could aim the key at a host it controls.
#[test]
fn platform_auto_approval_never_spends_a_provided_secret() {
    let handle = "secret_ref:v1:0123456789abcdef";
    assert!(params_carry_secret_handle(
        &serde_json::json!({"name": "x", "secret_key": handle})
    ));
    assert!(params_carry_secret_handle(
        &serde_json::json!({"extra": [{"token": handle}]})
    ));
    assert!(!params_carry_secret_handle(
        &serde_json::json!({"name": "x", "secret_key": "***", "port": 9000})
    ));

    // The marker must survive the credential redactor that sanitises approval
    // input before it is shown: a key named after "secret" would be masked to
    // "***" and silently read back as false.
    assert_eq!(
        redact_value(&serde_json::json!({"carries_handle": true}))["carries_handle"],
        serde_json::json!(true)
    );

    let uses_secret = PermissionRequest {
        id: "secret".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({"method": "POST", "carries_handle": true}),
    };
    assert!(automatic_platform_decision(&uses_secret).is_none());

    let plain = PermissionRequest {
        id: "plain".to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({"method": "POST", "carries_handle": false}),
    };
    assert!(matches!(
        automatic_platform_decision(&plain),
        Some(PermissionDecision::AllowTool)
    ));

    let plan_with_one_secret_step = PermissionRequest {
        id: "plan".to_string(),
        kind: PermissionKind::PlanApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({
            "steps": [
                {"method": "POST", "carries_handle": false},
                {"method": "POST", "carries_handle": true}
            ]
        }),
    };
    assert!(automatic_platform_decision(&plan_with_one_secret_step).is_none());
}

/// Double-resolve: the second remove returns None (409 semantic).
#[tokio::test]
async fn test_registry_double_resolve_returns_none() {
    use temps_ai::streaming::PermissionKind;
    use tokio::sync::oneshot;

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);

    let (tx, _rx) = oneshot::channel::<PermissionResolution>();
    svc.pending_permissions.lock().unwrap().insert(
        "req-dup".to_string(),
        PendingPermissionEntry {
            sender: tx,
            conv_public_id: "pub-dup".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "Bash".to_string(),
            input: serde_json::Value::Null,
            generation: uuid::Uuid::new_v4(),
            origin: PendingPermissionOrigin::Provider,
        },
    );

    // First claim succeeds.
    let first = svc.pending_permissions.lock().unwrap().remove("req-dup");
    assert!(first.is_some());
    drop(first); // entry (including sender) dropped

    // Second claim finds nothing (409 → 404 from the handler's perspective).
    let second = svc.pending_permissions.lock().unwrap().remove("req-dup");
    assert!(
        second.is_none(),
        "second remove must return None (already claimed)"
    );
}

/// Unknown id: remove returns None (404 semantic).
#[tokio::test]
async fn test_registry_unknown_id_returns_none() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);

    let result = svc.pending_permissions.lock().unwrap().remove("no-such-id");
    assert!(result.is_none(), "unknown id must return None");
}

#[test]
fn cancelled_permission_guard_removes_only_its_registration() {
    use temps_ai::streaming::PermissionKind;
    use tokio::sync::oneshot;

    let registry = Arc::new(Mutex::new(HashMap::new()));
    let old_generation = uuid::Uuid::new_v4();
    let newer_generation = uuid::Uuid::new_v4();
    let (sender, _receiver) = oneshot::channel();
    registry.lock().unwrap().insert(
        "shared-id".to_string(),
        PendingPermissionEntry {
            sender,
            conv_public_id: "conversation".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "temps".to_string(),
            input: serde_json::Value::Null,
            generation: newer_generation,
            origin: PendingPermissionOrigin::Provider,
        },
    );

    drop(PendingPermissionGuard {
        registry: registry.clone(),
        permission_id: "shared-id".to_string(),
        generation: old_generation,
    });
    assert_eq!(
        registry.lock().unwrap()["shared-id"].generation,
        newer_generation,
        "an older cancelled waiter must not delete a replacement"
    );

    drop(PendingPermissionGuard {
        registry: registry.clone(),
        permission_id: "shared-id".to_string(),
        generation: newer_generation,
    });
    assert!(registry.lock().unwrap().is_empty());
}

/// Drain-on-close: when the subprocess exits (simulate by dropping the receiver),
/// a synthetic deny sent to the still-pending sender propagates the "denied"
/// decision and unblocks any waiting task.
#[tokio::test]
async fn test_registry_drain_on_close_denies_pending() {
    use temps_ai::streaming::{PermissionDecision, PermissionKind};
    use tokio::sync::oneshot;

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);

    let (tx, rx) = oneshot::channel::<PermissionResolution>();
    svc.pending_permissions.lock().unwrap().insert(
        "req-drain".to_string(),
        PendingPermissionEntry {
            sender: tx,
            conv_public_id: "pub-drain".to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: "Bash".to_string(),
            input: serde_json::Value::Null,
            generation: uuid::Uuid::new_v4(),
            origin: PendingPermissionOrigin::Provider,
        },
    );

    // Simulate subprocess exit: drain the registry with a synthetic deny.
    let drained: Vec<_> = {
        let mut registry = svc.pending_permissions.lock().unwrap();
        registry.drain().collect()
    };
    assert_eq!(drained.len(), 1, "one entry must have been drained");
    let (_id, entry) = drained.into_iter().next().unwrap();
    let deny_result = entry.sender.send(PermissionResolution {
        decision: PermissionDecision::DenyTool {
            reason: Some("subprocess exited".to_string()),
        },
        auth: test_auth(),
        metadata: test_request_metadata(),
    });
    assert!(deny_result.is_ok(), "send on drained entry must succeed");

    let resolution = rx.await.unwrap();
    assert!(
        matches!(resolution.decision, PermissionDecision::DenyTool { reason: Some(ref r) } if r.contains("exited")),
        "drained entry must deliver deny"
    );
}

/// A turn that produces nothing must SAY so.
///
/// Before this, a provider failure just ended the round loop: the SSE stream
/// closed with zero events, so the UI showed the user's own message and then
/// nothing, forever — indistinguishable from a hang, and with no hint that
/// the provider key was the problem. A self-hosted operator has no support
/// channel to ask, so the error has to reach the screen.
#[tokio::test]
async fn empty_turn_emits_an_actionable_error_instead_of_silence() {
    // Every model call fails, including the tool-free salvage call.
    let ai = Arc::new(ScriptedAi::new(vec![
        Err(AiError::Provider {
            purpose: "chat.test.tools".to_string(),
            reason: "bad api key".to_string(),
        }),
        Err(AiError::Provider {
            purpose: "chat.test.tools.final".to_string(),
            reason: "bad api key".to_string(),
        }),
    ]));
    let provider = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools) = service_with(ai);

    let conv = test_conversation();
    let provider_dyn: Arc<dyn ConversationContextProvider> = provider;
    let mut stream = svc
        .try_tool_loop(&conv, vec![], Some(provider_dyn), tools, &test_auth())
        .await;

    let mut errors = Vec::new();
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => errors.push(e.to_string()),
        }
    }

    assert!(
        events.is_empty(),
        "a failed turn should stream no content, got {events:?}"
    );
    assert_eq!(errors.len(), 1, "expected exactly one error event");
    let msg = &errors[0];
    assert!(
        msg.contains("bad api key"),
        "the underlying provider error must survive to the user: {msg}"
    );
    assert!(
        msg.contains("selected harness/provider status"),
        "the error must point at the selected runtime status: {msg}"
    );
}

#[test]
fn live_wire_classifies_provider_failures_without_exposing_raw_diagnostics() {
    let event = wire_event_for(&Err(ChatError::Ai(
        "Invalid MCP configuration: EACCES open '/run/secrets/private.json' token=secret"
            .to_string(),
    )));
    let payload: serde_json::Value =
        serde_json::from_str(&event.data).expect("public failure is valid JSON");

    assert_eq!(event.event, "error");
    assert_eq!(payload["code"], "tool_configuration_unreadable");
    assert_eq!(payload["title"], "Application tools could not start");
    assert!(!event.data.contains("/run/secrets"));
    assert!(!event.data.contains("token=secret"));
}

#[tokio::test]
async fn harness_mcp_capability_is_authenticated_scoped_and_removed_with_guard() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_executor = calls.clone();
    let executor: ToolExecutor = Arc::new(move |call| {
        let calls = calls_for_executor.clone();
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(format!("scoped:{}", call.arguments))
        })
    });
    let tools = vec![ChatTool {
        name: "temps".to_string(),
        description: "Scoped platform read".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let interactions: temps_ai::InteractionExecutor =
        Arc::new(|_| Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) }));
    let (server, guard, _events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        tools,
        executor,
        None,
        interactions,
        Duration::from_secs(60),
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");

    let unauthorized = svc
        .handle_harness_mcp_request(
            bridge_id,
            "wrong-token",
            mcp_request!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
    assert_eq!(unauthorized, Err(HarnessMcpError::Unauthorized));

    let notification = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await
        .expect("authorized notification");
    assert!(notification.is_none());
    let ping = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        )
        .await
        .expect("authorized ping")
        .expect("null id still receives response");
    assert_eq!(ping.id, temps_ai::mcp::McpRequestId::Null);
    assert!(matches!(
        ping.result,
        Some(temps_ai::mcp::McpResult::Empty(_))
    ));
    let invalid_permission = svc
        .handle_harness_mcp_request(bridge_id, &server.authorization_token,
            mcp_request!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"temps_native_permission","arguments":{}}}))
        .await.expect("authorized capability").expect("invalid params response");
    assert_eq!(
        invalid_permission.error.expect("JSON-RPC error").code,
        -32602
    );

    let response = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "temps", "arguments": {"command": "--help"}}
            }),
        )
        .await
        .expect("authorized capability")
        .expect("request response");
    let response = serde_json::to_value(response).expect("MCP response JSON");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(response["result"]["isError"], false);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("--help")));

    drop(guard);
    let after_turn = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        )
        .await;
    assert_eq!(after_turn, Err(HarnessMcpError::NotFound));
}

#[tokio::test]
async fn expired_harness_mcp_capability_fails_closed() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
    let interactions: temps_ai::InteractionExecutor = Arc::new(|_| {
        Box::pin(async { Ok(temps_ai::PermissionDecision::DenyTool { reason: None }) })
    });
    let (server, _guard, _events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        Vec::new(),
        executor,
        None,
        interactions,
        Duration::ZERO,
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");

    let result = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
    assert_eq!(result, Err(HarnessMcpError::Expired));
}

#[tokio::test]
async fn harness_mcp_native_permission_round_trips_the_decision() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen_for_interaction = seen.clone();
    let interactions: temps_ai::InteractionExecutor = Arc::new(move |request| {
        *seen_for_interaction.lock().expect("capture permission") = Some(request);
        Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) })
    });
    let (server, _guard, _events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        Vec::new(),
        executor,
        None,
        interactions,
        Duration::from_secs(60),
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");

    let response = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "temps_native_permission",
                    "arguments": {
                        "tool_name": "Bash",
                        "input": {"command": "npm install"}
                    }
                }
            }),
        )
        .await
        .expect("authorized capability")
        .expect("request response");
    let response = serde_json::to_value(response).expect("MCP response JSON");

    let permission = seen
        .lock()
        .expect("captured permission")
        .clone()
        .expect("permission was requested");
    assert_eq!(permission.tool_name, "Bash");
    assert_eq!(permission.input["command"], "npm install");
    let payload: serde_json::Value = serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("permission payload text"),
    )
    .expect("permission payload JSON");
    assert_eq!(payload["behavior"], "allow");
    assert_eq!(payload["updatedInput"]["command"], "npm install");
}

#[tokio::test]
async fn harness_mcp_native_permission_returns_an_explicit_denial() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
    let interactions: temps_ai::InteractionExecutor = Arc::new(|_| {
        Box::pin(async {
            Ok(temps_ai::PermissionDecision::DenyTool {
                reason: Some("Denied in Temps".to_string()),
            })
        })
    });
    let (server, _guard, _events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        Vec::new(),
        executor,
        None,
        interactions,
        Duration::from_secs(60),
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");

    let response = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "temps_native_permission",
                    "arguments": {
                        "tool_name": "Write",
                        "input": {"file_path": "/workspace/denied.txt"}
                    }
                }
            }),
        )
        .await
        .expect("authorized capability")
        .expect("request response");
    let response = serde_json::to_value(response).expect("MCP response JSON");

    let payload: serde_json::Value = serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("permission payload text"),
    )
    .expect("permission payload JSON");
    assert_eq!(payload["behavior"], "deny");
    assert_eq!(payload["message"], "Denied in Temps");
    assert!(payload.get("updatedInput").is_none());
}

#[test]
fn managed_process_inputs_are_bounded_and_workspace_relative() {
    let valid = ToolCall {
        id: "call-1".to_string(),
        name: PROCESS_START_TOOL.to_string(),
        arguments: serde_json::json!({
            "name": "web",
            "program": "npm",
            "args": ["run", "dev"],
            "directory": "apps/web",
            "restart": true
        })
        .to_string(),
    };
    assert!(matches!(
        parse_runtime_process_operation(&valid),
        Ok(RuntimeProcessOperation::Start { name, .. }) if name == "web"
    ));

    for directory in ["/etc", "../outside", "apps/../../outside"] {
        let call = ToolCall {
            arguments: serde_json::json!({
                "name": "web", "program": "npm", "directory": directory
            })
            .to_string(),
            ..valid.clone()
        };
        assert!(
            parse_runtime_process_operation(&call).is_err(),
            "{directory}"
        );
    }

    let too_many_args = ToolCall {
        arguments: serde_json::json!({
            "name": "web",
            "program": "npm",
            "args": vec!["x"; MAX_PROCESS_ARGUMENTS + 1]
        })
        .to_string(),
        ..valid
    };
    assert!(parse_runtime_process_operation(&too_many_args).is_err());
}

#[test]
fn managed_process_rpc_ids_are_stable_bounded_and_capability_scoped() {
    let rpc_id = temps_ai::mcp::McpRequestId::String("request-42".into());
    let first = managed_process_call_id("bridge-a", &rpc_id).expect("valid rpc id");
    let retry = managed_process_call_id("bridge-a", &rpc_id).expect("stable retry id");
    let other_capability =
        managed_process_call_id("bridge-b", &rpc_id).expect("other capability id");
    assert_eq!(first, retry);
    assert_ne!(first, other_capability);
    assert!(first.starts_with("tmcp_"));
    assert!(managed_process_call_id("bridge-a", &temps_ai::mcp::McpRequestId::Null).is_err());
    assert!(managed_process_call_id(
        "bridge-a",
        &temps_ai::mcp::McpRequestId::String("x".repeat(257))
    )
    .is_err());
}

#[tokio::test]
async fn managed_process_mcp_emits_authoritative_call_before_bounded_result() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = Arc::new(db_service(db));
    let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
    let (release_tx, release_rx) = oneshot::channel();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let process_executor: ManagedProcessExecutor = Arc::new(move |call| {
        let release_rx = release_rx.clone();
        Box::pin(async move {
            assert_eq!(call.name, PROCESS_STATUS_TOOL);
            let receiver = release_rx
                .lock()
                .await
                .take()
                .expect("single process execution");
            receiver.await.expect("test releases process execution");
            Ok(serde_json::json!({"status":"running","token":"must-redact"}).to_string())
        })
    });
    let interactions: temps_ai::InteractionExecutor =
        Arc::new(|_| Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) }));
    let (server, _guard, mut events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        managed_process_tools(),
        executor,
        Some(process_executor),
        interactions,
        Duration::from_secs(60),
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");
    let request = mcp_request!({
        "jsonrpc":"2.0", "id":"stable-request", "method":"tools/call",
        "params":{"name":PROCESS_STATUS_TOOL,"arguments":{"process_id":"proc_1"}}
    });
    let request_service = svc.clone();
    let request_bridge_id = bridge_id.to_string();
    let request_token = server.authorization_token.clone();
    let request_task = tokio::spawn(async move {
        request_service
            .handle_harness_mcp_request(&request_bridge_id, &request_token, request)
            .await
    });
    let first = events.recv().await.expect("tool call event");
    let call_id = match first {
        ChatStreamDelta::ToolCall(call) => call.id,
        other => panic!("expected call before executor release, got {other:?}"),
    };
    release_tx.send(()).expect("release process executor");
    let second = events.recv().await.expect("tool result event");
    let result_id = match second {
        ChatStreamDelta::ToolResult {
            call: result_call,
            result,
        } => {
            assert!(!result.contains("must-redact"));
            result_call.id
        }
        other => panic!("unexpected managed process event order: {other:?}"),
    };
    assert_eq!(call_id, result_id);
    let response = request_task
        .await
        .expect("request task")
        .expect("authorized process request")
        .expect("process response");
    let response = serde_json::to_value(response).expect("MCP response JSON");
    assert_eq!(response["result"]["isError"], false);
    assert!(!response.to_string().contains("must-redact"));
}

#[test]
fn managed_process_echo_detection_accepts_qualified_provider_names_only() {
    assert!(is_managed_process_echo(PROCESS_START_TOOL));
    assert!(is_managed_process_echo(
        "mcp__temps-chat__temps_process_restart"
    ));
    assert!(is_managed_process_echo("temps-chat_temps_process_status"));
    assert!(is_managed_process_echo(&managed_process_display_name(
        PROCESS_STATUS_TOOL,
        true
    )));
    assert!(!is_managed_process_echo("prefix_temps_process_start"));
    assert!(!is_managed_process_echo("temps_process_start_extra"));
}

#[test]
fn authoritative_managed_process_names_are_qualified_for_display_only() {
    for raw in [
        PROCESS_START_TOOL,
        PROCESS_STATUS_TOOL,
        PROCESS_LOGS_TOOL,
        PROCESS_STOP_TOOL,
        PROCESS_RESTART_TOOL,
    ] {
        assert_eq!(
            managed_process_display_name(raw, true),
            format!("mcp__temps-chat__{raw}")
        );
        assert_eq!(managed_process_display_name(raw, false), raw);
        assert!(is_managed_process_echo(&managed_process_display_name(
            raw, true
        )));
    }
    assert_eq!(managed_process_display_name("Read", true), "Read");
    assert_eq!(
        managed_process_display_name("mcp__temps-chat__temps_process_status", true),
        "mcp__temps-chat__temps_process_status"
    );
}

#[test]
fn managed_process_results_redact_exact_turn_secrets_and_remain_bounded() {
    let secret = "opaque-local-value-123".to_string();
    let response = temps_ai::RuntimeProcessResponse::Logs {
        process_id: "proc_1".to_string(),
        lines: vec![temps_ai::RuntimeProcessLogLine {
            sequence: 1,
            timestamp_ms: 2,
            stream: "stdout".to_string(),
            text: format!("server printed {secret}"),
        }],
        next_sequence: None,
        truncated: false,
    };
    let encoded = serialize_process_result(&response, std::slice::from_ref(&secret))
        .expect("bounded process response");
    assert!(!encoded.contains(&secret));
    assert!(encoded.contains("***"));
    assert!(encoded.len() <= MAX_PROCESS_RESULT_BYTES);
}

#[test]
fn managed_process_errors_are_structured_without_raw_provider_details() {
    let raw = temps_ai::AiError::Provider {
        purpose: "chat.runtime_process".to_string(),
        reason: "socket=/run/private.sock token=do-not-leak".to_string(),
    };
    let encoded = managed_process_error_text(&raw);
    let value: serde_json::Value =
        serde_json::from_str(&encoded).expect("structured process error");
    assert_eq!(value["is_error"], true);
    assert!(!encoded.contains("private.sock"));
    assert!(!encoded.contains("do-not-leak"));

    let adapter_error = temps_ai::AiError::Provider {
        purpose: "chat.application.process.start".to_string(),
        reason:
            "this sandbox runtime must be upgraded; existing process proc_123 token=do-not-leak"
                .to_string(),
    };
    let sanitized = sanitize_runtime_process_error(adapter_error, &["do-not-leak".to_string()]);
    let encoded = managed_process_error_text(&sanitized);
    assert!(encoded.contains("must be upgraded"));
    assert!(encoded.contains("proc_123"));
    assert!(!encoded.contains("do-not-leak"));
}

#[tokio::test]
async fn managed_process_does_not_execute_when_authoritative_event_queue_is_full() {
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let svc = db_service(db);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let process_calls = calls.clone();
    let process_executor: ManagedProcessExecutor = Arc::new(move |_| {
        process_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok("{}".to_string()) })
    });
    let executor: ToolExecutor = Arc::new(|_| Box::pin(async { Ok("unused".to_string()) }));
    let interactions: temps_ai::InteractionExecutor =
        Arc::new(|_| Box::pin(async { Ok(temps_ai::PermissionDecision::AllowTool) }));
    let (server, _guard, _events) = ConversationService::register_harness_mcp(
        svc.harness_mcp_entries.clone(),
        "http://host.docker.internal:8080",
        7,
        managed_process_tools(),
        executor,
        Some(process_executor),
        interactions,
        Duration::from_secs(60),
    );
    let bridge_id = server
        .url
        .split("/sandbox-tools/")
        .nth(1)
        .and_then(|suffix| suffix.split('/').next())
        .expect("bridge id in scoped URL");
    let entry = svc
        .harness_mcp_entries
        .lock()
        .expect("MCP registry")
        .get(bridge_id)
        .cloned()
        .expect("registered MCP entry");
    for _ in 0..32 {
        entry
            .event_tx
            .try_send(ChatStreamDelta::Text("occupied".to_string()))
            .expect("fill bounded event queue");
    }

    let response = svc
        .handle_harness_mcp_request(
            bridge_id,
            &server.authorization_token,
            mcp_request!({
                "jsonrpc":"2.0", "id":8, "method":"tools/call",
                "params":{"name":PROCESS_STATUS_TOOL,"arguments":{"process_id":"proc_1"}}
            }),
        )
        .await
        .expect("authorized capability")
        .expect("queue error response");
    let response = serde_json::to_value(response).expect("MCP response JSON");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// A provider whose reply stream is cut short after `partial_text` (if any)
/// with the typed interruption error the gateway raises when the connection
/// drops, the client times out, or no terminal chunk arrives.
struct InterruptedStreamAi {
    partial_text: Option<&'static str>,
    cause: &'static str,
}

#[async_trait]
impl AiService for InterruptedStreamAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }

    async fn chat_stream_turn(&self, _request: ChatTurnRequest) -> Result<ChatTurnStream, AiError> {
        let partial_text = self.partial_text;
        let cause = self.cause;
        let stream = async_stream::stream! {
            if let Some(text) = partial_text {
                yield Ok(ChatStreamDelta::Text(text.to_string()));
            }
            yield Err(AiError::StreamInterrupted {
                purpose: "chat.test.tools".to_string(),
                reason: cause.to_string(),
            });
        };
        Ok(Box::pin(stream))
    }
}

async fn collect_all(
    stream: Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, ChatError>> + Send>>,
) -> Vec<Result<ChatStreamEvent, ChatError>> {
    let mut stream = stream;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

async fn run_interrupted_turn(
    partial_text: Option<&'static str>,
    cause: &'static str,
) -> Vec<Result<ChatStreamEvent, ChatError>> {
    let ai: Arc<dyn AiService> = Arc::new(InterruptedStreamAi {
        partial_text,
        cause,
    });
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools) = service_with(ai);
    let stream = svc
        .try_tool_loop(
            &test_conversation(),
            vec![],
            Some(provider),
            tools,
            &test_auth(),
        )
        .await;
    collect_all(stream).await
}

#[tokio::test]
async fn interrupted_stream_keeps_the_partial_text_and_reports_a_typed_failure() {
    let events = run_interrupted_turn(
        Some("Once upon a"),
        "the provider closed the stream before sending a final chunk",
    )
    .await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(ChatStreamEvent::Token(text)) if text == "Once upon a"
        )),
        "the partial text must still reach the client: {events:?}"
    );
    let errors: Vec<&ChatError> = events
        .iter()
        .filter_map(|event| event.as_ref().err())
        .collect();
    assert_eq!(errors.len(), 1, "exactly one failure event: {events:?}");
    match errors[0] {
        ChatError::StreamInterrupted {
            partial_bytes,
            cause,
            turn_id,
            ..
        } => {
            assert_eq!(*partial_bytes, "Once upon a".len());
            assert!(cause.contains("before sending a final chunk"), "{cause}");
            assert!(!turn_id.is_empty());
        }
        other => panic!("expected StreamInterrupted, got {other:?}"),
    }
    let message = errors[0].to_string();
    assert!(message.contains("conversation"), "{message}");
    assert!(message.contains("11 bytes of partial text"), "{message}");
}

#[tokio::test]
async fn interrupted_stream_with_no_text_reports_only_the_interruption() {
    let events = run_interrupted_turn(None, "the connection failed mid-reply: timed out").await;

    let errors: Vec<&ChatError> = events
        .iter()
        .filter_map(|event| event.as_ref().err())
        .collect();
    assert_eq!(errors.len(), 1, "{events:?}");
    assert!(
        matches!(
            errors[0],
            ChatError::StreamInterrupted {
                partial_bytes: 0,
                ..
            }
        ),
        "{errors:?}"
    );
    assert!(
        !errors[0].to_string().contains("no reply was produced"),
        "the interruption replaces the generic empty-turn error"
    );
}

#[tokio::test]
async fn complete_stream_reports_no_failure() {
    let ai = Arc::new(ScriptedAi::new(vec![Ok(vec![
        ChatStreamDelta::Text("Hello, ".to_string()),
        ChatStreamDelta::Text("world".to_string()),
    ])]));
    let provider: Arc<dyn ConversationContextProvider> = Arc::new(StubProvider {
        tool_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let (svc, tools, auth) = service_with_current_tool_auth(ai);
    let stream = svc
        .try_tool_loop(&test_conversation(), vec![], Some(provider), tools, &auth)
        .await;
    let events = collect_all(stream).await;

    assert!(
        events.iter().all(Result::is_ok),
        "a finished reply must not report a failure: {events:?}"
    );
    assert_eq!(
        joined_text(
            &events
                .into_iter()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        ),
        "Hello, world"
    );
}

#[test]
fn stream_interruption_public_failure_is_safe_and_retryable() {
    let error = ChatError::StreamInterrupted {
        conversation_id: 7,
        turn_id: "turn-1".to_string(),
        partial_bytes: 12,
        cause: "error sending request for url (https://provider.internal.example/v1)".to_string(),
    };
    let failure = error.public_failure();

    assert_eq!(failure.code, "provider_stream_interrupted");
    assert!(failure.retryable);
    assert!(failure.detail.contains("marked incomplete"));
    assert!(
        !failure.detail.contains("provider.internal.example"),
        "the raw transport cause must stay in the server log: {}",
        failure.detail
    );

    let empty = ChatError::StreamInterrupted {
        conversation_id: 7,
        turn_id: "turn-1".to_string(),
        partial_bytes: 0,
        cause: "timed out".to_string(),
    };
    assert!(empty
        .public_failure()
        .detail
        .contains("nothing was received"));
}

#[test]
fn interrupted_message_metadata_is_flagged_partial() {
    let failure = ChatError::StreamInterrupted {
        conversation_id: 7,
        turn_id: "turn-1".to_string(),
        partial_bytes: 3,
        cause: "eof".to_string(),
    }
    .public_failure();
    let parts = vec![serde_json::json!({"type": "text", "text": "abc"})];

    let flagged = assistant_message_metadata_with(&[], &parts, false, Some(&failure))
        .expect("metadata is produced");
    assert_eq!(flagged["interrupted"], serde_json::json!(true));
    assert_eq!(
        flagged["interruption"]["code"],
        serde_json::json!("provider_stream_interrupted")
    );
    assert_eq!(flagged["parts"], serde_json::json!(parts));

    let normal = assistant_message_metadata(&[], &parts, false).expect("metadata");
    assert!(normal.get("interrupted").is_none());
    assert!(normal.get("interruption").is_none());
}

// ---------------------------------------------------------------------------
// Focused tests for the helper modules split out of this file: each entry
// point the service calls is exercised directly.
// ---------------------------------------------------------------------------

/// A completion-only `AiService` for the title helpers: answers every
/// `complete` with `reply` and records the system prompt and prompt it saw.
struct TitleAi {
    reply: Result<String, ()>,
    requests: Mutex<Vec<(Option<String>, String)>>,
}

impl TitleAi {
    fn replying(text: &str) -> Arc<Self> {
        Arc::new(Self {
            reply: Ok(text.to_string()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            reply: Err(()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<(Option<String>, String)> {
        self.requests.lock().expect("title requests").clone()
    }
}

#[async_trait]
impl AiService for TitleAi {
    async fn is_available(&self) -> bool {
        true
    }

    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        assert_eq!(request.purpose, "chat.title");
        self.requests
            .lock()
            .expect("title requests")
            .push((request.system.clone(), request.prompt.clone()));
        match &self.reply {
            Ok(text) => Ok(AiResponse {
                text: text.clone(),
                json: None,
                model: "title-model".to_string(),
            }),
            Err(()) => Err(AiError::NotAvailable),
        }
    }

    async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        Err(AiError::NotAvailable)
    }
}

fn title_message(id: i64, role: &str, content: &str) -> ai_messages::Model {
    ai_messages::Model {
        id,
        conversation_id: 1,
        role: role.to_string(),
        content: content.to_string(),
        metadata: None,
        tokens_in: None,
        tokens_out: None,
        cost_microcents: None,
        created_at: Utc::now(),
    }
}

fn logged_statements(db: Arc<DatabaseConnection>) -> Vec<sea_orm::Statement> {
    Arc::try_unwrap(db)
        .unwrap_or_else(|_| panic!("the test holds the only database handle"))
        .into_transaction_log()
        .into_iter()
        .flat_map(|transaction| transaction.statements().to_vec())
        .collect()
}

#[tokio::test]
async fn a_generated_title_is_cleaned_and_stored_as_automatic() {
    let mut conversation = test_conversation();
    conversation.metadata = Some(serde_json::json!({ "focus_project_id": 7 }));
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[conversation.clone()], [conversation]])
            .into_connection(),
    );
    let ai = TitleAi::replying("\n\"Deploy The Shop.\"\nextra reasoning");
    let ai_service: Arc<dyn AiService> = ai.clone();

    generate_and_store_title(
        &ai_service,
        &db,
        1,
        Some(7),
        "gateway".to_string(),
        "gpt-4o-mini".to_string(),
        "can you deploy my shop?",
    )
    .await;

    let requests = ai.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .0
        .as_deref()
        .is_some_and(|system| system.contains("the user's first message")));
    assert!(requests[0].1.contains("can you deploy my shop?"));
    let statements = logged_statements(db);
    let update = statements
        .iter()
        .find(|statement| statement.sql.starts_with("UPDATE \"ai_conversations\""))
        .expect("the title is stored");
    let bound = format!("{:?}", update.values);
    assert!(bound.contains("Deploy The Shop"), "{bound}");
    assert!(
        bound.contains(r#""title_source": String("auto")"#),
        "{bound}"
    );
    assert!(bound.contains("focus_project_id"), "{bound}");
}

#[tokio::test]
async fn a_failed_or_empty_title_completion_stores_nothing() {
    for ai in [TitleAi::failing(), TitleAi::replying("  \n \"\" ")] {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let ai_service: Arc<dyn AiService> = ai.clone();

        generate_and_store_title(
            &ai_service,
            &db,
            1,
            None,
            "gateway".to_string(),
            "gpt-4o-mini".to_string(),
            "hello",
        )
        .await;

        assert_eq!(ai.requests().len(), 1);
        assert!(logged_statements(db).is_empty());
    }
}

#[tokio::test]
async fn a_title_the_user_chose_is_never_refreshed() {
    let mut conversation = test_conversation();
    conversation.metadata = Some(with_title_source(None, "user"));
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([[conversation]])
            .into_connection(),
    );
    let ai = TitleAi::replying("Something Else");
    let ai_service: Arc<dyn AiService> = ai.clone();

    refresh_title_from_conversation(
        &ai_service,
        &db,
        1,
        Some(7),
        "gateway".to_string(),
        "gpt-4o-mini".to_string(),
    )
    .await;

    assert!(ai.requests().is_empty());
    let statements = logged_statements(db);
    assert_eq!(statements.len(), 1, "{statements:?}");
    assert!(statements[0].sql.starts_with("SELECT"));
}

#[tokio::test]
async fn the_title_is_refreshed_from_the_conversation_only_at_the_configured_turns() {
    let at_first_turn = vec![
        title_message(1, "user", "hi, can you help me?"),
        title_message(2, "assistant", "Sure, what are we building?"),
    ];
    let conversation = test_conversation();
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![conversation.clone()]])
            .append_query_results([at_first_turn])
            .append_query_results([
                vec![conversation.clone()],
                vec![conversation.clone()],
                vec![conversation],
            ])
            .into_connection(),
    );
    let ai = TitleAi::replying("Getting Started");
    let ai_service: Arc<dyn AiService> = ai.clone();

    refresh_title_from_conversation(
        &ai_service,
        &db,
        1,
        Some(7),
        "gateway".to_string(),
        "gpt-4o-mini".to_string(),
    )
    .await;

    let requests = ai.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .0
        .as_deref()
        .is_some_and(|system| system.contains("summarizing the whole conversation")));
    assert!(requests[0].1.contains("User: hi, can you help me?"));
    assert!(requests[0]
        .1
        .contains("Assistant: Sure, what are we building?"));
    let statements = logged_statements(db);
    assert!(statements.iter().any(|statement| statement
        .sql
        .starts_with("UPDATE \"ai_conversations\"")
        && format!("{:?}", statement.values).contains("Getting Started")));

    // Two user turns is not one of TITLE_REFRESH_USER_TURNS: no completion.
    let at_second_turn = vec![
        title_message(1, "user", "hi"),
        title_message(2, "assistant", "hello"),
        title_message(3, "user", "deploy the shop"),
    ];
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![test_conversation()]])
            .append_query_results([at_second_turn])
            .into_connection(),
    );
    let ai = TitleAi::replying("Unused");
    let ai_service: Arc<dyn AiService> = ai.clone();

    refresh_title_from_conversation(
        &ai_service,
        &db,
        1,
        Some(7),
        "gateway".to_string(),
        "gpt-4o-mini".to_string(),
    )
    .await;

    assert!(ai.requests().is_empty());
    assert!(logged_statements(db)
        .iter()
        .all(|statement| statement.sql.starts_with("SELECT")));
}

#[test]
fn turn_events_reach_live_viewers_even_after_the_stream_reader_left() {
    #[allow(
        clippy::disallowed_methods,
        reason = "test: emit_turn_event takes the turn's unbounded sender; two events are sent"
    )]
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();
    let (live, mut live_rx) = broadcast::channel::<WireEvent>(4);

    emit_turn_event(&tx, &live, Ok(ChatStreamEvent::Token("Hello".to_string())));
    drop(rx);
    emit_turn_event(&tx, &live, Ok(ChatStreamEvent::Token(" world".to_string())));

    let first = live_rx.try_recv().expect("first live event");
    let second = live_rx.try_recv().expect("second live event");
    assert_eq!(
        (first.event.as_str(), first.data.as_str()),
        ("token", "Hello")
    );
    assert_eq!(
        (second.event.as_str(), second.data.as_str()),
        ("token", " world")
    );
}

#[test]
fn turn_events_are_forwarded_to_the_stream_reader_without_live_viewers() {
    #[allow(
        clippy::disallowed_methods,
        reason = "test: emit_turn_event takes the turn's unbounded sender; one event is sent"
    )]
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamEvent, ChatError>>();
    let (live, live_rx) = broadcast::channel::<WireEvent>(4);
    drop(live_rx);

    emit_turn_event(&tx, &live, Ok(ChatStreamEvent::Token("Hi".to_string())));

    assert_eq!(
        rx.try_recv().expect("forwarded event").expect("ok event"),
        ChatStreamEvent::Token("Hi".to_string())
    );
}

fn context_usage_fixture() -> PersistedContextWindowUsage {
    PersistedContextWindowUsage {
        used_tokens: 1_234,
        limit_tokens: Some(200_000),
        model: Some("model-a".to_string()),
        source: "provider_reported".to_string(),
        estimated: false,
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

#[tokio::test]
async fn context_usage_is_written_only_for_the_owning_turn() {
    let mut conversation = test_conversation();
    conversation.metadata = Some(serde_json::json!({ "last_failure": { "code": "kept" } }));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([[conversation]])
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();

    persist_context_usage(&db, 1, Some("turn-1"), &context_usage_fixture())
        .await
        .expect("context usage persists");

    let statements = logged_statements(Arc::new(db));
    let update = statements
        .iter()
        .find(|statement| statement.sql.starts_with("UPDATE \"ai_conversations\""))
        .expect("context usage is written");
    assert!(update.sql.contains("active_turn_id"));
    let bound = format!("{:?}", update.values);
    assert!(bound.contains("turn-1"), "{bound}");
    assert!(bound.contains("1234"), "{bound}");
    assert!(bound.contains("kept"), "{bound}");
}

#[tokio::test]
async fn context_usage_for_a_missing_conversation_writes_nothing() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<ai_conversations::Model>::new()])
        .into_connection();

    persist_context_usage(&db, 404, None, &context_usage_fixture())
        .await
        .expect("a missing conversation is not an error");

    let statements = logged_statements(Arc::new(db));
    assert_eq!(statements.len(), 1, "{statements:?}");
    assert!(statements[0].sql.starts_with("SELECT"));
}

#[tokio::test]
async fn context_usage_propagates_a_database_error() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
        .into_connection();

    let error = persist_context_usage(&db, 1, None, &context_usage_fixture())
        .await
        .expect_err("the database error is returned");

    assert!(error.to_string().contains("connection reset"), "{error}");
}

#[tokio::test]
async fn an_interrupted_assistant_message_is_persisted_with_its_failure() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let failure = ChatError::Ai("provider stream ended early".to_string()).public_failure();

    persist_assistant_message_with(
        &db,
        42,
        "Partial answer",
        &[],
        &[],
        "Partial answer",
        false,
        Some(&failure),
    )
    .await
    .expect("the partial message persists");

    let statements = logged_statements(Arc::new(db));
    assert_eq!(statements.len(), 1);
    assert!(statements[0].sql.starts_with("UPDATE \"ai_messages\""));
    let bound = format!("{:?}", statements[0].values);
    assert!(bound.contains("Partial answer"), "{bound}");
    assert!(bound.contains("interrupted"), "{bound}");
}

#[test]
fn only_the_five_process_tools_are_managed_process_tools() {
    let tools = managed_process_tools();
    assert_eq!(tools.len(), 5);
    assert!(tools.iter().all(|tool| is_managed_process_tool(&tool.name)));
    for other in [
        "temps",
        TEMPS_WRITE_TOOL_NAME,
        "mcp__temps-chat__temps_process_start",
        "temps_process_start_extra",
        "",
    ] {
        assert!(!is_managed_process_tool(other), "{other}");
    }
}

#[test]
fn invalid_process_requests_are_reported_as_validation_errors() {
    let error = invalid_process_request("the managed process id is invalid");

    match &error {
        AiError::Provider { purpose, reason } => {
            assert_eq!(purpose, "chat.runtime_process.validation");
            assert_eq!(reason, "the managed process id is invalid");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let shown: serde_json::Value =
        serde_json::from_str(&managed_process_error_text(&error)).expect("error JSON");
    assert_eq!(shown["is_error"], true);
    assert_eq!(
        shown["error"],
        "The managed process request is invalid. Correct its fields and try again."
    );
}

#[test]
fn prompt_composition_logging_accepts_empty_and_populated_prompts() {
    let conversation = test_conversation();
    log_prompt_composition(&conversation, &[], &[]);
    log_prompt_composition(
        &conversation,
        &[
            ChatMessage::system("## Rules\nBe brief.".to_string()),
            user_msg("hi"),
        ],
        &managed_process_tools(),
    );
}
