// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tool-call dispatch for the agentic loop: tool names and result
//! presentation, the incident and repository guards, the conversation, ask-user,
//! sandbox-deploy and write-proposal tools, and their audit records.

use super::*;

/// Tool name for the write-proposal (confirm-gated) tool.
pub(super) const TEMPS_WRITE_TOOL_NAME: &str = "temps_write";

/// Tool name for goal tracking (assistant chats).
pub(super) const TEMPS_GOAL_TOOL_NAME: &str = "temps_goal";

fn is_temps_write_tool_name(name: &str) -> bool {
    matches!(name, TEMPS_WRITE_TOOL_NAME | "mcp__temps-chat__temps_write")
}

/// Most a visible API-tool result may occupy in the live stream and in the
/// persisted transcript. The model still gets the full result for the turn.
pub(super) const MAX_PUBLIC_API_RESULT_BYTES: usize = 16 * 1024;

fn truncate_for_display(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… truncated, {} of {} bytes shown",
        &text[..end],
        end,
        text.len()
    )
}

/// Client-visible tool results must not contain raw data fetched through
/// server-side credentials. The model keeps the full result for reasoning.
///
/// The `temps` API tool is different: it replays the request through the real
/// router with the caller's own permissions, over an allowlist that excludes
/// secret-bearing reads, so the user is entitled to everything it returns. They
/// see it redacted (credential-looking keys and strings masked) and capped.
/// Every other tool, such as repository reads made with server-side provider
/// credentials, shows only a status.
pub(super) fn public_tool_result(name: &str, result: &str) -> String {
    if is_temps_write_tool_name(name) {
        return redact_json_string(result);
    }
    if name == TEMPS_GOAL_TOOL_NAME {
        return redact_json_string(result);
    }
    if crate::workspace_chats::is_blueprint_tool(name) {
        // The proposal itself (parts and flows, never credentials: the
        // document is checked for them) is what the console draws as the
        // architecture card.
        return redact_json_string(result);
    }
    if name == crate::ask_user::ASK_USER_TOOL {
        // The questions and the user's own answers: the transcript keeps them
        // so the exchange survives a reload.
        return redact_json_string(result);
    }
    if name == crate::secret_slots::REQUEST_SECRET_TOOL {
        // The descriptor (handle, label, field) is what the console renders as
        // the private form. It carries no value, so it is shown as returned
        // rather than run through the credential redactor, which exists for
        // results that may hold one and could mask the handle the card needs.
        return result.to_string();
    }
    if name == "temps" || crate::sandbox_link::is_sandbox_tool(name) {
        return truncate_for_display(redact_json_string(result), MAX_PUBLIC_API_RESULT_BYTES);
    }

    "Tool completed; detailed result is withheld from the chat transcript.".to_string()
}

/// A proposal exists only when the write tool returned a receipt during this
/// turn. Model prose is not authority: accepting an older receipt or a sentence
/// that merely says "Proposal staged" would strand the user without a durable
/// action or confirmation card.
pub(super) fn has_fresh_proposal_receipt(tools: &[serde_json::Value]) -> bool {
    tools.iter().any(|tool| {
        let Some(name) = tool.get("name").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !is_temps_write_tool_name(name) {
            return false;
        }
        let Some(result) = tool.get("result").and_then(serde_json::Value::as_str) else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(result)
            .ok()
            .and_then(|receipt| {
                receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .map(|status| matches!(status, "proposed" | "proposed_plan"))
            })
            .unwrap_or(false)
    })
}

/// Detect only direct assertions, not explanatory prose that happens to quote
/// the phrase. This keeps the postcondition narrow while covering the wording
/// used by the write-tool contract and common harness responses.
pub(super) fn claims_proposal_was_staged(content: &str) -> bool {
    let claim = content
        .trim_start_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '#' | '*' | '_' | '`' | '>')
        })
        .to_ascii_lowercase();
    [
        "proposal staged",
        "proposal has been staged",
        "the proposal is staged",
        "i staged the proposal",
        "i've staged the proposal",
    ]
    .iter()
    .any(|prefix| claim.starts_with(prefix))
}

// ---------------------------------------------------------------------------
// Write-tool dispatch helper (free function so the spawned task can borrow it)
// ---------------------------------------------------------------------------

/// Did this tool result come from actually EXECUTING `operation_id` successfully?
///
/// Judged from the result, never from the arguments the model sent. Matching on
/// the request is how a safety control gets satisfied without doing anything:
/// `alerts preview_alert --help` mentions the operation, returns cheerful help
/// text, and would otherwise count as a backtest — the model could then propose
/// a threshold it never checked, and the human confirming would see it
/// described as verified.
///
/// The read CLI's execution path returns `{"operation":…,"status":…,"data":…}`,
/// so the operation that ran and the status it returned are both stated by the
/// executor. Help and every error return plain prose, which parses as nothing
/// and is correctly rejected.
fn executed_operation_ok(result: &str, operation_id: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(result) else {
        return false;
    };
    if v.get("operation").and_then(serde_json::Value::as_str) != Some(operation_id) {
        return false;
    }
    v.get("status")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|s| (200..300).contains(&s))
}

/// Cap what is kept for repeat-detection and the backtest check.
///
/// These two uses need to know *that* a call happened and roughly what came
/// back, not the whole payload — and a tool result can be 128 KiB. Retaining
/// every one at full size for the length of a turn would hold megabytes per
/// concurrent chat, and would quietly undo [`trim_carried_tool_results`], since
/// the repeat guard feeds this copy back into the transcript.
pub(super) fn summarize_for_recall(result: &str) -> String {
    const MAX: usize = 4 * 1024;
    if result.len() <= MAX {
        return result.to_string();
    }
    // Cut on a char boundary — `result` is arbitrary tool output, so slicing
    // blind would panic on any multi-byte character straddling the limit.
    let mut end = MAX;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[… truncated for recall; re-run the command if you need the rest]",
        &result[..end]
    )
}

/// Did a tool call produce something the model can reason from, or only a
/// rejection?
///
/// This is the difference between a turn that is making progress and one that
/// is stuck, and the two need opposite treatment: a long productive task should
/// be allowed to continue, a loop of validation errors should be stopped
/// quickly. A single round counter cannot tell them apart, so it ends up too
/// tight for the first and far too loose for the second.
///
/// The read CLI has a defined success shape — `{"operation":…,"status":N,…}` —
/// so classifying it is parsing a contract, not guessing. Anything else is
/// assumed productive: a tool this function does not understand must not be
/// mistaken for a failure and used to cut a working turn short.
pub(super) fn tool_result_is_productive(result: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(result) {
        Ok(v) => match v.get("status").and_then(serde_json::Value::as_u64) {
            Some(status) => (200..300).contains(&status),
            // Valid JSON without a status: the write tool's proposal receipt,
            // which is very much progress.
            None => true,
        },
        // Not JSON — every failure path returns a plain sentence, but so could a
        // future tool, hence the explicit allow-list of known rejections rather
        // than treating all prose as failure.
        Err(_) => {
            // Kept next to the messages they mirror. If a rejection is reworded
            // without updating this, the effect is that a stuck turn runs longer
            // (bounded by MAX_ROUNDS and the deadline) rather than a working turn
            // being cut short — the safe direction, but still worth keeping true.
            const REJECTIONS: &[&str] = &[
                "Unknown parameter(s)",
                "Missing required parameters",
                "has the wrong shape",
                "Required parameter",
                "Unknown operation",
                "Unknown command",
                "is not available",
                "authorization could not be verified",
                TOOL_CALL_CAP_REJECTION,
                "You already ran",
                "Not staged",
                "Invalid `temps_write` arguments",
            ];
            !REJECTIONS.iter().any(|r| result.contains(r)) && !result.contains("` failed:")
        }
    }
}

/// Operations that must not be proposed without first backtesting them, mapped
/// to the read-tool operation that does the backtesting.
///
/// A threshold is a number someone invented; the only thing that turns it into
/// a judgement is knowing how often it would have fired. The system prompt asks
/// for that and a model will still skip it — and worse, claim it did — so this
/// is enforced rather than requested. `preview_alert` is read-only and saves
/// nothing, so requiring it costs one extra round and no risk.
const BACKTEST_REQUIRED: &[(&str, &str)] = &[
    ("create_alert", "preview_alert"),
    ("update_alert", "preview_alert"),
];

/// If `command` proposes an operation that requires a backtest, and no matching
/// backtest was run this turn, return the message to send back instead.
///
/// Matching is by operation name only, not by exact arguments: the model
/// legitimately tunes a threshold between backtest and proposal, and demanding
/// byte-identical config would just make the requirement impossible to satisfy.
/// The goal is that the model has *looked*, not that it never adjusted after.
///
/// The check is deliberately scoped to the CURRENT turn. A follow-up like "now
/// create that alert" therefore backtests again, which reads like friction but
/// is the correct behaviour: it is a new proposal, the metric has moved since,
/// and a backtest from an earlier turn says nothing about the threshold being
/// proposed now. Honouring an older one would keep the requirement satisfiable
/// while quietly voiding what it guarantees — and the re-run is read-only and
/// costs a single round.
pub(super) fn missing_backtest(
    command: &str,
    seen_calls: &std::collections::HashMap<String, String>,
) -> Option<String> {
    // Match the operation in COMMAND POSITION only. The CLI form is
    // `[section] <operation> --flags…`, so scanning every token would also fire
    // on an operation name that merely appears inside an argument (a rule named
    // "create_alert", say) and demand a backtest for an unrelated call.
    let position = command
        .split_whitespace()
        .take_while(|t| !t.starts_with("--"))
        .take(2);
    let (op, backtest_op) = position
        .filter_map(|token| BACKTEST_REQUIRED.iter().find(|(op, _)| token == *op))
        .next()?;

    let ran_backtest = seen_calls
        .values()
        .any(|result| executed_operation_ok(result, backtest_op));
    if ran_backtest {
        return None;
    }

    // Spell the command out rather than describing it. The model reliably
    // rediscovers these flags by trial and error otherwise, and every wrong
    // guess costs a round out of the loop's budget — enough of them and the
    // turn ends with nothing proposed, which is a worse outcome than the
    // un-backtested proposal this gate exists to prevent.
    Some(format!(
        "Not staged: `{op}` requires a backtest first, and none succeeded this turn.\n\n\
         Run this with the `temps` tool, substituting the values you intend to propose:\n\n\
         alerts {backtest_op} --metric_name <metric> --aggregation <avg|max|p95|…> \
         --window_secs <secs> \
         --detection_config '{{\"kind\":\"static\",\"comparator\":\"gt\",\"threshold\":<number>}}'\n\n\
         It is read-only and saves nothing. It returns `breach_count` out of the points it \
         scored — how often this exact rule WOULD have fired. Judge the threshold by it (firing \
         on nearly every bucket is noise; never firing may be pointless), then propose the \
         configuration you settled on.\n\n\
         Do not describe a rule as backtested unless you have run this and read the result."
    ))
}

#[derive(Default)]
pub(super) struct ToolExecutionState {
    pub(super) goals: Option<crate::goals::GoalToolSupport>,
    pub(super) secrets: Option<crate::secret_slots::SecretToolSupport>,
    /// Present for assistant chats on a server with a sandbox backend.
    pub(super) sandbox: Option<crate::sandbox_link::SandboxLinkService>,
    /// Present for assistant chats when workspaces are wired.
    pub(super) blueprints: Option<crate::workspace_chats::BlueprintToolSupport>,
    /// An architecture was proposed this turn: the user decides before
    /// anything is built, so the turn may still read but no longer act.
    pub(super) blueprint_proposed: bool,
    pub(super) proposed_action_ids: Vec<i64>,
    pub(super) seen_calls: std::collections::HashMap<String, String>,
    /// Set when the chat belongs to an autofix incident (ADR-054): the
    /// tools such a chat may not run are refused at dispatch, whichever
    /// execution path (gateway rounds or harness executor) made the call.
    pub(super) incident: Option<IncidentToolGuard>,
}

/// The incident a chat belongs to, and where its refusals are recorded.
pub(super) struct IncidentToolGuard {
    pub(super) public_id: String,
    pub(super) db: Arc<DatabaseConnection>,
}

/// Refuse a tool an incident chat may not run, recording the attempt on the
/// incident's timeline. `None` when the call may proceed.
async fn refuse_in_incident(guard: Option<&IncidentToolGuard>, call: &ToolCall) -> Option<String> {
    let guard = guard?;
    if !crate::autofix::tool_refused_in_incident(&call.name) {
        return None;
    }
    let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
        .map(|value| redact_value(&value))
        .unwrap_or(serde_json::Value::Null);
    tracing::info!(
        incident_id = %guard.public_id,
        tool = %call.name,
        "refused a tool an autofix incident chat may not run"
    );
    crate::autofix::record_refused_action(guard.db.as_ref(), &guard.public_id, &call.name, input)
        .await;
    Some(crate::autofix::refused_tool_message(&call.name))
}

/// Whether this caller may drive the Git repo-exploration tools
/// (`read_repo_file`, `list_repo_dir`, `list_repo_branches`, `list_repo_tags`).
///
/// These tools read the project's private source through the stored Git
/// provider connection token, and their raw output is streamed straight back
/// to the caller in a `tool_result` SSE frame before anything could filter it.
/// The chat endpoints themselves only require `ProjectsRead`/`ProjectsWrite`,
/// so without this check a caller with no Git permission at all could ask the
/// model to read `.env`, credentials or any source file and get the bytes
/// verbatim — the repository permission would be enforced on
/// `/git/repositories/*` and nowhere else.
pub(super) fn caller_may_use_repo_tools(auth: &AuthContext) -> bool {
    auth.has_permission(&temps_auth::permissions::Permission::GitRepositoriesRead)
}

/// Re-check live project membership in addition to the instance-level Git
/// permission. Conversation ownership intentionally survives membership
/// changes so the transcript remains available, but private source access must
/// not. Checker failures deny access rather than exposing repository data.
pub(super) async fn project_repo_tools_allowed(
    auth: &AuthContext,
    project_id: Option<i32>,
    checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
) -> bool {
    if !caller_may_use_repo_tools(auth) {
        return false;
    }
    let Some(project_id) = project_id else {
        return false;
    };
    if auth.is_admin() || auth.has_role(&temps_auth::permissions::Role::PlatformAdmin) {
        return true;
    }
    let Some(checker) = checker else {
        // OSS has no team membership layer; the instance permission remains
        // the canonical authorization decision in that configuration.
        return true;
    };
    let Some(user_id) = auth.user_id_opt() else {
        return false;
    };
    match checker
        .effective_project_permissions(user_id, project_id)
        .await
    {
        Ok(Some(permissions)) => permissions.iter().any(|permission| {
            permission == &temps_auth::permissions::Permission::GitRepositoriesRead.to_string()
        }),
        Ok(None) => match checker.user_can_access_project(user_id, project_id).await {
            Ok(allowed) => allowed,
            Err(error) => {
                tracing::error!(
                    user_id,
                    project_id,
                    error = %error,
                    "failed to verify live project membership for AI repository tool"
                );
                false
            }
        },
        Err(error) => {
            tracing::error!(
                user_id,
                project_id,
                error = %error,
                "failed to verify live project repository permission for AI repository tool"
            );
            false
        }
    }
}

/// What the model may still do in a turn after it proposed an architecture:
/// read the platform and its playbooks, and revise the proposal itself.
pub(super) fn may_follow_a_proposal(name: &str) -> bool {
    name == "temps"
        || name == crate::harness::PLAYBOOK_TOOL
        || name == crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL
        || name == crate::sandbox_link::SANDBOX_READ_TOOL
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_conversation_tool(
    call: &ToolCall,
    project_id: Option<i32>,
    conversation_id: i64,
    context_id: &str,
    auth: &AuthContext,
    project_access_checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
    context_provider: Option<&Arc<dyn ConversationContextProvider>>,
    api_tools: Option<&Arc<dyn ConversationContextProvider>>,
    repo_tools: Option<&Arc<dyn ConversationContextProvider>>,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending: Option<&PendingActionService>,
    audit: Option<&dyn AuditLogger>,
    interactions: Option<&PlatformInteractionExecutor>,
    state: &mut ToolExecutionState,
) -> String {
    // First, before the repeat guard or any routing: an incident chat's
    // refused tools never run, however the call reached dispatch.
    if let Some(refusal) = refuse_in_incident(state.incident.as_ref(), call).await {
        return refusal;
    }
    let call_key = format!("{}|{}", call.name, call.arguments.trim());
    // Shell commands and file reads legitimately repeat (`git status`, a
    // `curl` against the server just started) because the sandbox changes
    // between calls, so the repeat guard does not apply to them.
    let repeatable = crate::sandbox_link::is_sandbox_tool(&call.name);
    if let Some(previous) = state.seen_calls.get(&call_key).filter(|_| !repeatable) {
        return format!(
            "You already ran `{}` with these exact arguments earlier this turn. \
             Do NOT repeat it. Use a different next step or answer from the previous result:\n\n{}",
            call.name, previous
        );
    }
    let result = if state.blueprint_proposed && !may_follow_a_proposal(&call.name) {
        format!(
            "Not run: you proposed an architecture in this turn, and the user decides on it \
             before anything is built or changed. End your turn now: say in one or two \
             sentences why the design is shaped this way and that you will start once they \
             accept it. (`{}` was not called.)",
            call.name
        )
    } else if call.name == TEMPS_WRITE_TOOL_NAME {
        dispatch_write_tool(
            &call.arguments,
            project_id,
            conversation_id,
            context_id,
            auth,
            write_handle,
            pending,
            audit,
            interactions,
            &mut state.proposed_action_ids,
            &state.seen_calls,
        )
        .await
    } else if call.name == crate::harness::PLAYBOOK_TOOL {
        crate::harness::dispatch(&call.arguments)
    } else if call.name == crate::secret_slots::REQUEST_SECRET_TOOL {
        match &state.secrets {
            Some(secrets) => {
                secrets
                    .dispatch(&call.arguments, conversation_id, auth)
                    .await
            }
            None => "Secret requests are not available in this chat.".to_string(),
        }
    } else if call.name == crate::ask_user::ASK_USER_TOOL {
        dispatch_ask_user_tool(&call.arguments, conversation_id, interactions).await
    } else if crate::workspace_chats::is_blueprint_tool(&call.name) {
        match &state.blueprints {
            Some(blueprints) => {
                let result = blueprints
                    .dispatch(&call.name, &call.arguments, conversation_id, auth)
                    .await;
                if call.name == crate::workspace_chats::PROPOSE_BLUEPRINT_TOOL
                    && crate::workspace_chats::is_proposal_receipt(&result)
                {
                    state.blueprint_proposed = true;
                }
                result
            }
            None => {
                "Workspaces and architecture blueprints are not available in this chat.".to_string()
            }
        }
    } else if call.name == TEMPS_GOAL_TOOL_NAME {
        match &state.goals {
            Some(goals) => goals.dispatch(&call.arguments, conversation_id, auth).await,
            None => "Goal tracking is not available in this chat.".to_string(),
        }
    } else if call.name == crate::sandbox_link::SANDBOX_CREATE_TOOL
        && project_id.is_some()
        && !project_repo_tools_allowed(auth, project_id, project_access_checker).await
    {
        // Defence in depth, as for the repository tools: creating a sandbox
        // in a project chat clones the project's private source.
        format!(
            "Tool '{}' is not available: it requires current project access and the {} permission.",
            call.name,
            temps_auth::permissions::Permission::GitRepositoriesRead
        )
    } else if call.name == crate::sandbox_link::SANDBOX_DEPLOY_TOOL {
        match &state.sandbox {
            Some(sandbox) => {
                dispatch_sandbox_deploy_tool(
                    sandbox,
                    &call.arguments,
                    project_id,
                    conversation_id,
                    auth,
                    project_access_checker,
                    interactions,
                )
                .await
            }
            None => "Sandboxes are not available in this chat.".to_string(),
        }
    } else if crate::sandbox_link::is_sandbox_tool(&call.name) {
        match &state.sandbox {
            Some(sandbox) => {
                sandbox
                    .dispatch(&call.name, &call.arguments, conversation_id, auth)
                    .await
            }
            None => "Sandboxes are not available in this chat.".to_string(),
        }
    } else if call.name == "temps" {
        if let Some(provider) = api_tools {
            provider
                .execute_tool_with_auth(project_id, context_id, &call.name, &call.arguments, auth)
                .await
        } else {
            "Tool 'temps' is not available (API tools provider absent).".to_string()
        }
    } else if matches!(
        call.name.as_str(),
        "read_repo_file" | "list_repo_dir" | "list_repo_branches" | "list_repo_tags"
    ) {
        if !project_repo_tools_allowed(auth, project_id, project_access_checker).await {
            // Defence in depth: the tool was never offered to the model for
            // this caller, but a model can still emit the call name from
            // memory, and dispatch must not honour it.
            format!(
                "Tool '{}' is not available: it requires current project access and the {} permission.",
                call.name,
                temps_auth::permissions::Permission::GitRepositoriesRead
            )
        } else if let Some(provider) = repo_tools {
            provider
                .execute_tool_with_auth(project_id, context_id, &call.name, &call.arguments, auth)
                .await
        } else {
            format!(
                "Tool '{}' is not available (repo tools provider absent).",
                call.name
            )
        }
    } else if let Some(provider) = context_provider {
        provider
            .execute_tool(project_id, context_id, &call.name, &call.arguments)
            .await
    } else {
        format!("Tool '{}' is not available in this context.", call.name)
    };
    if !repeatable {
        state
            .seen_calls
            .insert(call_key, summarize_for_recall(&result));
    }
    result
}

/// Show the model's questions on the chat's question card and wait for the
/// user's answers. Questions are never answered automatically, in any
/// permission mode; when nobody answers before the turn ends the model is told
/// so instead of being handed an invented answer.
async fn dispatch_ask_user_tool(
    arguments: &str,
    conversation_id: i64,
    interactions: Option<&PlatformInteractionExecutor>,
) -> String {
    use crate::ask_user::{self, ASK_USER_TOOL};
    let questions = match ask_user::parse(arguments) {
        Ok(questions) => questions,
        Err(message) => return message,
    };
    let Some(interactions) = interactions else {
        return ask_user::unanswered_result("questions cannot be shown in this turn");
    };
    let request = PermissionRequest {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: PermissionKind::Question,
        tool_name: ASK_USER_TOOL.to_string(),
        input: ask_user::card_input(&questions),
    };
    match interactions(request).await {
        Ok(PermissionResolution {
            decision: PermissionDecision::AnswerQuestion { answers },
            ..
        }) => {
            let answers = ask_user::answers(&questions, &answers);
            tracing::info!(
                conversation_id,
                questions = answers.len(),
                blank = answers
                    .iter()
                    .filter(|answer| answer.answer.is_empty())
                    .count(),
                "user answered assistant question"
            );
            ask_user::answered_result(&answers)
        }
        Ok(_) => ask_user::unanswered_result("the question was dismissed"),
        Err(error) => {
            tracing::info!(
                conversation_id,
                %error,
                "assistant question ended without an answer"
            );
            ask_user::unanswered_result(
                "the question was not answered before the turn ended or was stopped",
            )
        }
    }
}

/// Dispatch a `temps_write` tool call: parse and validate the command, request
/// inline approval, execute with the approving user's current authorization,
/// and return the real execution result to the model in the same turn.
///
/// Returns a readable string result that goes back to the model as the tool
/// result — always, even on internal errors (never panics).
#[allow(clippy::too_many_arguments)]
/// Deploy the chat sandbox's files as a project's source (no Git). The files
/// are packed first so the approval card can say what will be uploaded; the
/// deployment itself runs only after the user approves it (automatically in the
/// modes that approve changes), under the approver's own permissions.
async fn dispatch_sandbox_deploy_tool(
    sandbox: &crate::sandbox_link::SandboxLinkService,
    arguments: &str,
    chat_project_id: Option<i32>,
    conversation_id: i64,
    auth: &AuthContext,
    project_access_checker: Option<&Arc<dyn temps_core::ProjectAccessChecker>>,
    interactions: Option<&PlatformInteractionExecutor>,
) -> String {
    use crate::sandbox_deploy::{authorize_deploy, SANDBOX_DEPLOY_TOOL};
    let Some(interactions) = interactions else {
        return format!(
            "`{SANDBOX_DEPLOY_TOOL}` cannot ask for approval in this turn. Nothing was deployed."
        );
    };
    let prepared = match sandbox
        .prepare_deploy(arguments, conversation_id, auth)
        .await
    {
        Ok(prepared) => prepared,
        Err(message) => return message,
    };
    let project_id = prepared.args.project_id;
    if let Err(message) =
        authorize_deploy(auth, project_id, chat_project_id, project_access_checker).await
    {
        return message;
    }
    let request = temps_ai::streaming::PermissionRequest {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: PermissionKind::ToolApproval,
        tool_name: SANDBOX_DEPLOY_TOOL.to_string(),
        input: serde_json::json!({
            "operation": "deploy_sandbox_source",
            "method": "POST",
            "summary": prepared.summary(),
            "project_id": project_id,
            "parameters": {
                "environment_id": prepared.args.environment_id,
                "directory": prepared.directory,
                "files": prepared.packed.files,
                "left_out": prepared.packed.left_out,
            },
            "required_permission": temps_auth::permissions::Permission::DeploymentsCreate.to_string(),
        }),
    };
    let resolution = match interactions(request).await {
        Ok(resolution) => resolution,
        Err(error) => {
            return format!(
                "Approval to deploy to project {project_id} ended before it was resolved: \
                 {error}. Nothing was deployed."
            )
        }
    };
    match resolution.decision {
        PermissionDecision::AllowTool => {
            // The approver may be a different user than the one who asked.
            if let Err(message) = authorize_deploy(
                &resolution.auth,
                project_id,
                chat_project_id,
                project_access_checker,
            )
            .await
            {
                return message;
            }
            sandbox
                .deploy_prepared(prepared, &resolution.auth, conversation_id, chat_project_id)
                .await
        }
        PermissionDecision::DenyTool { reason } => format!(
            "The user declined to deploy to project {project_id}{}. Nothing was deployed; \
             acknowledge that and ask what they would like instead.",
            reason
                .as_deref()
                .map(|reason| format!(": {}", redact_text(reason)))
                .unwrap_or_default()
        ),
        _ => format!(
            "The approval for deploying to project {project_id} was not granted. Nothing was \
             deployed."
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_write_tool(
    arguments: &str,
    project_id: Option<i32>,
    conversation_id: i64,
    context_id: &str,
    auth: &AuthContext,
    write_handle: Option<&temps_ai_api_tools::InternalApiCaller>,
    pending_svc: Option<&PendingActionService>,
    audit: Option<&dyn AuditLogger>,
    interactions: Option<&PlatformInteractionExecutor>,
    proposed_action_ids: &mut Vec<i64>,
    seen_calls: &std::collections::HashMap<String, String>,
) -> String {
    let caller = match write_handle {
        Some(c) => c,
        None => {
            return "The `temps_write` tool is not available (write caller not yet wired)."
                .to_string()
        }
    };
    let pending = match pending_svc {
        Some(p) => p,
        None => {
            return "The `temps_write` tool is not available (pending-action service absent)."
                .to_string()
        }
    };
    let interactions = match interactions {
        Some(interactions) => interactions,
        None => {
            return "The `temps_write` tool cannot request approval in this turn. No change was created or executed."
                .to_string()
        }
    };

    // Parse the JSON arguments.
    let args: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Invalid `temps_write` arguments (not JSON): {e}"),
    };
    let requested_project_id = match args.get("project_id").and_then(serde_json::Value::as_i64) {
        Some(value) => match i32::try_from(value) {
            Ok(value) => Some(value),
            Err(_) => return "The selected project_id is outside the supported range.".to_string(),
        },
        None => None,
    };
    let application_scope = context_id.starts_with("app_") && context_id.contains(':');
    let global_scope = crate::provider::is_cross_project_context(context_id);
    let (selected_project_id, project_scope) = if application_scope {
        // A workspace is a machine and context boundary, not an authorization
        // boundary for the Temps control plane. Project links describe which
        // source trees and data networks are mounted in this workspace. The
        // approving user's live RBAC and project membership are enforced by
        // the internal API call itself, so global resources and any currently
        // authorized project remain operable from every workspace chat.
        (
            requested_project_id,
            requested_project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    } else if global_scope {
        (
            requested_project_id,
            requested_project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    } else {
        if requested_project_id.is_some() && requested_project_id != project_id {
            return "Cross-project selection is available only in a workspace or global thread."
                .to_string();
        }
        (
            project_id,
            project_id.map_or(ProjectSelectorScope::Unrestricted, |project_id| {
                ProjectSelectorScope::Allowed(vec![project_id])
            }),
        )
    };
    let scope = ApiCallScope {
        auth: auth.clone(),
        project_scope,
        origin: None,
        secrets: None,
    };

    // Two shapes: a single `command` (standalone action) or an ordered
    // `commands` array (a multi-step *plan*, confirmed one step at a time in
    // order). Use a plan when order matters — e.g. change resources THEN redeploy.
    let is_plan = args.get("commands").is_some();
    let commands: Vec<String> = if let Some(arr) = args.get("commands").and_then(|v| v.as_array()) {
        let cmds: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if cmds.is_empty() {
            return "The `temps_write` 'commands' array is empty — provide one command \
                    string per step, in execution order."
                .to_string();
        }
        cmds
    } else if let Some(c) = args.get("command").and_then(|v| v.as_str()) {
        vec![c.to_string()]
    } else {
        return "The `temps_write` tool requires either a 'command' string (one action) \
                or a 'commands' array (an ordered multi-step plan). Use `--help` to \
                discover operations."
            .to_string();
    };

    // Refuse to stage anything that should have been backtested and wasn't.
    // Checked before `prepare_write_cli` so the model gets the one instruction
    // that matters rather than a validation error it would fix and re-submit,
    // still un-backtested.
    for cmd in &commands {
        if let Some(msg) = missing_backtest(cmd, seen_calls) {
            return msg;
        }
    }

    // Prepare (validate, NO execution) every step first. If any step is a help
    // request or fails to validate, surface that and stage NOTHING — a plan is
    // only proposed once every step is valid.
    let mut prepared_steps: Vec<(temps_ai_api_tools::PreparedWrite, Option<String>)> = Vec::new();
    for (i, cmd) in commands.iter().enumerate() {
        match caller.prepare_write_cli(cmd, &scope) {
            WritePrepareOutcome::Help(text) => return text,
            WritePrepareOutcome::Invalid(msg) => {
                return if is_plan {
                    format!("Plan not staged — step {} is invalid: {msg}", i + 1)
                } else {
                    msg
                };
            }
            WritePrepareOutcome::Prepared(prepared) => {
                let perm = prepared.required_permission.clone();
                prepared_steps.push((prepared, perm));
            }
        }
    }

    // Store the encrypted action first so the approval is durable and the
    // eventual execution has an immutable, auditable request. Unlike the old
    // detached proposal flow, the active tool call now waits for the inline
    // decision and returns the execution result to the model in this turn.
    if !is_plan {
        let (prepared, perm) = &prepared_steps[0];
        let row = match pending
            .create_inline(
                conversation_id,
                selected_project_id,
                prepared,
                perm.clone(),
                auth.user_id(),
            )
            .await
        {
            Ok(row) => row,
            Err(e) => return format!("Could not prepare this change for approval: {e}"),
        };
        proposed_action_ids.push(row.id);
        let mut lifecycle = InlineActionGuard::new(pending, vec![row.public_id.clone()]);
        let request = temps_ai::streaming::PermissionRequest {
            id: uuid::Uuid::new_v4().simple().to_string(),
            kind: PermissionKind::ToolApproval,
            tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
            input: serde_json::json!({
                "operation": row.operation_id,
                "method": row.method,
                "summary": row.summary,
                "project_id": row.project_id,
                "parameters": redact_value(&prepared.params),
                "carries_handle": params_carry_secret_handle(&prepared.params),
                "required_permission": perm,
                "action_id": row.public_id,
            }),
        };
        let resolution = match interactions(request).await {
            Ok(resolution) => resolution,
            Err(error) => {
                return format!(
                    "Approval for `{}` ended before it was resolved: {error}",
                    row.operation_id
                )
            }
        };
        return match resolution.decision {
            PermissionDecision::AllowTool => {
                match pending
                    .confirm_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                        Some(approver_origin(&resolution)),
                    )
                    .await
                {
                    Ok(updated) => {
                        lifecycle.disarm();
                        audit_confirmed_action(audit, &resolution, &updated).await;
                        pending_action_tool_result(&updated)
                    }
                    Err(error) => serde_json::json!({
                        "status": "failed",
                        "action_id": row.public_id,
                        "operation": row.operation_id,
                        "error": redact_text(&error.to_string()),
                        "instruction": "The approved platform action failed. Diagnose this error, inspect current state if useful, and either explain the blocker or propose a corrected action."
                    })
                    .to_string(),
                }
            }
            PermissionDecision::DenyTool { ref reason } => {
                match pending
                    .reject_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                {
                    Ok(updated) => {
                        lifecycle.disarm();
                        audit_rejected_action(audit, &resolution, &updated).await;
                        serde_json::json!({
                            "status": "rejected",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "reason": reason.as_ref().map(|value| redact_text(value)),
                            "instruction": "The user rejected this action. Acknowledge the decision and use their reason when choosing a safer next step."
                        })
                        .to_string()
                    }
                    Err(error) => {
                        audit_action_transition_failed(
                            audit,
                            &resolution,
                            &row,
                            "reject",
                            &error.to_string(),
                        )
                        .await;
                        serde_json::json!({
                            "status": "failed",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "error": redact_text(&error.to_string()),
                            "instruction": "The rejection could not be recorded, so this action was cancelled and was not executed. Explain that the platform could not persist the decision."
                        })
                        .to_string()
                    }
                }
            }
            _ => serde_json::json!({
                "status": "failed",
                "action_id": row.public_id,
                "operation": row.operation_id,
                "error": "The approval response did not match this tool request."
            })
            .to_string(),
        };
    }

    // Multi-step plans use one inline plan approval. Approval executes the
    // immutable steps in order; the first failure halts the remainder and is
    // returned to the model so it can react without a separate user message.
    let rows = match pending
        .create_inline_plan(
            conversation_id,
            selected_project_id,
            &prepared_steps,
            auth.user_id(),
        )
        .await
    {
        Ok(rows) => rows,
        Err(e) => return format!("Could not prepare this plan for approval: {e}"),
    };
    proposed_action_ids.extend(rows.iter().map(|row| row.id));
    let mut lifecycle = InlineActionGuard::new(
        pending,
        rows.iter().map(|row| row.public_id.clone()).collect(),
    );
    let plan_text = rows
        .iter()
        .map(|row| {
            format!(
                "{}. **{}** — {}",
                row.step_index + 1,
                row.operation_id,
                row.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let request = temps_ai::streaming::PermissionRequest {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: PermissionKind::PlanApproval,
        tool_name: TEMPS_WRITE_TOOL_NAME.to_string(),
        input: serde_json::json!({
            "plan": plan_text,
            "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
            "project_id": selected_project_id,
            "steps": rows.iter().zip(prepared_steps.iter()).map(|(row, (prepared, _))| serde_json::json!({
                "step": row.step_index + 1,
                "action_id": row.public_id,
                "operation": row.operation_id,
                "method": row.method,
                "summary": row.summary,
                "parameters": redact_value(&prepared.params),
                "carries_handle": params_carry_secret_handle(&prepared.params),
                "required_permission": row.required_permission,
            })).collect::<Vec<_>>()
        }),
    };
    let resolution = match interactions(request).await {
        Ok(resolution) => resolution,
        Err(error) => return format!("Plan approval ended before it was resolved: {error}"),
    };
    match resolution.decision {
        PermissionDecision::ApprovePlan => {
            let mut results = Vec::with_capacity(rows.len());
            let mut transition_failed = false;
            for row in &rows {
                match pending
                    .confirm_inline(
                        row.project_id,
                        &row.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                        Some(approver_origin(&resolution)),
                    )
                    .await
                {
                    Ok(updated) => {
                        audit_confirmed_action(audit, &resolution, &updated).await;
                        let failed = updated.status == "failed";
                        results.push(pending_action_result_value(&updated));
                        if failed {
                            break;
                        }
                    }
                    Err(error) => {
                        transition_failed = true;
                        results.push(serde_json::json!({
                            "status": "failed",
                            "action_id": row.public_id,
                            "operation": row.operation_id,
                            "error": redact_text(&error.to_string()),
                        }));
                        break;
                    }
                }
            }
            // `confirm_inline` terminally transitions every attempted row and
            // skips the remainder after the first failure.
            if !transition_failed {
                lifecycle.disarm();
            }
            let failed = results.iter().any(|result| {
                result.get("status").and_then(serde_json::Value::as_str) == Some("failed")
            });
            serde_json::json!({
                "status": if failed { "failed" } else { "executed" },
                "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                "steps": results,
                "instruction": if failed {
                    "The approved plan halted because a step failed. Diagnose the returned error and propose only the correction that is still needed."
                } else {
                    "The approved plan executed successfully. Summarize the concrete outcome."
                }
            })
            .to_string()
        }
        PermissionDecision::RejectPlan { ref feedback } => {
            let rejection = if let Some(first) = rows.first() {
                pending
                    .reject_inline(
                        first.project_id,
                        &first.public_id,
                        &resolution.auth,
                        Some(resolution.auth.user_id()),
                    )
                    .await
                    .map(Some)
            } else {
                Ok(None)
            };
            match rejection {
                Ok(updated) => {
                    lifecycle.disarm();
                    if let Some(updated) = &updated {
                        audit_rejected_action(audit, &resolution, updated).await;
                    }
                    serde_json::json!({
                        "status": "rejected",
                        "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                        "feedback": feedback.as_ref().map(|value| redact_text(value)),
                        "instruction": "The user rejected this plan. Acknowledge their feedback and do not execute or restage it unchanged."
                    })
                    .to_string()
                }
                Err(error) => {
                    if let Some(first) = rows.first() {
                        audit_action_transition_failed(
                            audit,
                            &resolution,
                            first,
                            "reject_plan",
                            &error.to_string(),
                        )
                        .await;
                    }
                    serde_json::json!({
                        "status": "failed",
                        "plan_id": rows.first().and_then(|row| row.plan_public_id.clone()),
                        "error": redact_text(&error.to_string()),
                        "instruction": "The plan rejection could not be recorded. The inline actions were cancelled and none were executed. Explain the persistence failure."
                    })
                    .to_string()
                }
            }
        }
        _ => serde_json::json!({
            "status": "failed",
            "error": "The approval response did not match this plan request."
        })
        .to_string(),
    }
}

fn pending_action_result_value(
    action: &temps_entities::ai_pending_actions::Model,
) -> serde_json::Value {
    serde_json::json!({
        "status": action.status,
        "action_id": action.public_id,
        "operation": action.operation_id,
        "method": action.method,
        "summary": action.summary,
        "result": action.result,
        "error": action.error,
    })
}

pub(super) fn pending_action_tool_result(
    action: &temps_entities::ai_pending_actions::Model,
) -> String {
    let mut value = pending_action_result_value(action);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "instruction".to_string(),
            serde_json::Value::String(if action.status == "failed" {
                "The approved platform action failed. Diagnose the returned error, inspect current state if useful, and either explain the blocker or propose a corrected action."
            } else {
                "The approved platform action executed successfully. Summarize the concrete outcome."
            }.to_string()),
        );
    }
    value.to_string()
}

/// The client that approved an action, so the audit trail of the API call the
/// assistant then makes names the approver's address and browser rather than
/// the `unknown` of an in-process request.
fn approver_origin(resolution: &PermissionResolution) -> temps_core::ai_tool_call::AiToolOrigin {
    temps_core::ai_tool_call::AiToolOrigin {
        ip_address: resolution.metadata.ip_address.clone(),
        user_agent: resolution.metadata.user_agent.clone(),
    }
}

async fn audit_confirmed_action(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
) {
    let Some(audit) = audit else { return };
    let event = AiActionConfirmedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
        status: action.status.clone(),
    };
    if let Err(error) = audit.create_audit_log(&event).await {
        tracing::error!(action_id = %action.public_id, "failed to audit inline AI action: {error}");
    }
}

async fn audit_rejected_action(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
) {
    let Some(audit) = audit else { return };
    let event = AiActionRejectedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
    };
    if let Err(error) = audit.create_audit_log(&event).await {
        tracing::error!(action_id = %action.public_id, "failed to audit rejected inline AI action: {error}");
    }
}

async fn audit_action_transition_failed(
    audit: Option<&dyn AuditLogger>,
    resolution: &PermissionResolution,
    action: &temps_entities::ai_pending_actions::Model,
    attempted_transition: &str,
    error: &str,
) {
    let Some(audit) = audit else { return };
    let event = AiActionTransitionFailedAudit {
        context: AuditContext {
            user_id: resolution.auth.user_id(),
            ip_address: Some(resolution.metadata.ip_address.clone()),
            user_agent: resolution.metadata.user_agent.clone(),
        },
        project_id: action.project_id,
        action_id: action.public_id.clone(),
        operation_id: action.operation_id.clone(),
        attempted_transition: attempted_transition.to_string(),
        error: redact_text(error),
    };
    if let Err(audit_error) = audit.create_audit_log(&event).await {
        tracing::error!(
            action_id = %action.public_id,
            %audit_error,
            "failed to audit inline AI action transition failure"
        );
    }
}
