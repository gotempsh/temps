// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An agent run on Temps' own tool loop (ADR-115 D2).
//!
//! The model comes from the AI gateway ([`AiService::chat`]); the agent works
//! in its run's sandbox through three tools, the same ones a chat with a
//! linked sandbox has: `sandbox_exec`, `sandbox_read_file` and
//! `sandbox_write_file`. No coding CLI is installed, signed in or driven.
//!
//! The loop ends when the model answers without calling a tool (its final
//! message), when it has used `max_turns` model turns, or when the run's time
//! budget is spent. Every step is written as one JSON line, so the run's log
//! streams live and the stored transcript replays it:
//!
//! ```text
//! {"type":"temps.text","text":"…"}
//! {"type":"temps.tool_call","id":"…","name":"sandbox_exec","input":{…}}
//! {"type":"temps.tool_result","id":"…","output":"…","is_error":false}
//! {"type":"temps.finish","reason":"stop"|"max_turns","turns":3}
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use temps_ai::{AiService, ChatMessage, ChatTool, ChatTurnRequest, ToolCall};
use temps_sandbox_providers::SandboxExecResult;
use tokio::time::Instant;

use crate::error::AgentError;
use crate::services::sandbox_registry::SandboxRegistry;
use temps_agent_core::OnEventCallback;

pub const SANDBOX_EXEC_TOOL: &str = "sandbox_exec";
pub const SANDBOX_READ_TOOL: &str = "sandbox_read_file";
pub const SANDBOX_WRITE_TOOL: &str = "sandbox_write_file";

/// The provider every agent turn is routed to: the AI gateway.
pub const GATEWAY_PROVIDER: &str = "gateway";

/// Most characters of one tool result the model sees. A longer result keeps
/// its head and its tail, where commands print what went wrong.
const MAX_TOOL_OUTPUT_CHARS: usize = 16_000;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;
const MAX_COMMAND_TIMEOUT_SECS: u64 = 600;
/// How long past its own `timeout` a command may take before the loop stops
/// waiting for the sandbox to answer.
const COMMAND_GRACE: Duration = Duration::from_secs(15);
const DEFAULT_READ_LINES: usize = 500;
const MAX_READ_LINES: usize = 2_000;
const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// The sandbox an agent run works in. [`RegistrySandbox`] is the real one;
/// tests use an in-memory one.
#[async_trait]
pub trait RunSandbox: Send + Sync {
    /// Absolute work directory that relative paths resolve against.
    fn work_dir(&self) -> &str;
    async fn exec(&self, cmd: Vec<String>) -> Result<SandboxExecResult, AgentError>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, AgentError>;
    async fn write_file(&self, path: &str, contents: &[u8], mode: u32) -> Result<(), AgentError>;
}

/// A run's sandbox in the [`SandboxRegistry`].
pub struct RegistrySandbox {
    registry: Arc<SandboxRegistry>,
    run_id: i32,
    work_dir: String,
}

impl RegistrySandbox {
    pub async fn new(registry: Arc<SandboxRegistry>, run_id: i32) -> Result<Self, AgentError> {
        let handle = registry.get(run_id).await?;
        let work_dir = handle.work_dir.to_string_lossy().into_owned();
        Ok(Self {
            registry,
            run_id,
            work_dir,
        })
    }
}

#[async_trait]
impl RunSandbox for RegistrySandbox {
    fn work_dir(&self) -> &str {
        &self.work_dir
    }

    async fn exec(&self, cmd: Vec<String>) -> Result<SandboxExecResult, AgentError> {
        self.registry
            .exec(self.run_id, cmd, Default::default(), None)
            .await
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, AgentError> {
        self.registry.read_file(self.run_id, path).await
    }

    async fn write_file(&self, path: &str, contents: &[u8], mode: u32) -> Result<(), AgentError> {
        self.registry
            .write_file(self.run_id, path, contents, mode)
            .await
    }
}

/// What one agent run asks of the loop.
pub struct AgentLoopRequest {
    pub run_id: i32,
    pub project_id: i32,
    /// A gateway model id; `None` uses the gateway's default model.
    pub model: Option<String>,
    pub prompt: String,
    /// Most model turns (one gateway call each), at least 1.
    pub max_turns: u32,
    /// The whole run's time budget.
    pub timeout: Duration,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoopOutcome {
    /// The run's transcript as JSON lines (see the module docs).
    pub transcript: String,
    /// The model's final message, or its last text when it ran out of turns.
    pub final_text: String,
    pub turns: u32,
    /// True when the run used every turn before giving a final message.
    pub exhausted_turns: bool,
}

/// The system prompt every agent run starts with.
pub fn system_prompt(work_dir: &str) -> String {
    format!(
        "You are a Temps agent working on a software project. The project's repository is \
         checked out at {work_dir} in an isolated sandbox. Work only through your tools: \
         `{SANDBOX_EXEC_TOOL}` runs a shell command, `{SANDBOX_READ_TOOL}` reads a file and \
         `{SANDBOX_WRITE_TOOL}` writes one. Look before you change anything, keep changes to \
         what the task needs, and run the project's own checks (tests, type checks, linters) \
         when it has them. Do not commit or push: Temps does that once you finish. When you \
         are done, answer with your final message and call no tool."
    )
}

/// The tools an agent run may call.
pub fn tools() -> Vec<ChatTool> {
    vec![
        ChatTool {
            name: SANDBOX_EXEC_TOOL.to_string(),
            description: "Run a shell command in the sandbox with `sh -c`, in the work \
                directory unless `cwd` is set. Returns the exit code, stdout and stderr. A \
                non-zero exit code is a result, not a tool failure: read it and decide what \
                to do."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to run." },
                    "cwd": { "type": "string", "description": "Working directory. Defaults to the work directory." },
                    "timeout_secs": { "type": "integer", "description": "Give up after this many seconds (1 to 600, default 120)." }
                },
                "required": ["command"]
            }),
        },
        ChatTool {
            name: SANDBOX_READ_TOOL.to_string(),
            description: "Read a text file. Relative paths resolve against the work \
                directory. Returns at most `limit` lines from `offset`, and says where to \
                continue; prefer `sandbox_exec` with `grep -n` to find a region of a big file."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path." },
                    "offset": { "type": "integer", "description": "First line to return, counting from 1. Defaults to 1." },
                    "limit": { "type": "integer", "description": "Most lines to return (default 500, at most 2000)." }
                },
                "required": ["path"]
            }),
        },
        ChatTool {
            name: SANDBOX_WRITE_TOOL.to_string(),
            description: "Write a text file, replacing any existing file. Relative paths \
                resolve against the work directory. At most 1 MiB per call."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path." },
                    "content": { "type": "string", "description": "Full file content." },
                    "executable": { "type": "boolean", "description": "Mark the file executable." }
                },
                "required": ["path", "content"]
            }),
        },
    ]
}

/// Run one agent to its final message, its turn cap or its deadline.
///
/// `on_event` receives every transcript line as it happens (the run's live
/// log). A failed model turn ends the run with [`AgentError::AgentModelFailed`];
/// a failed tool call is told to the model, which decides what to do next.
pub async fn run_agent_loop(
    ai: &dyn AiService,
    sandbox: &dyn RunSandbox,
    request: AgentLoopRequest,
    on_event: Option<OnEventCallback>,
) -> Result<AgentLoopOutcome, AgentError> {
    let deadline = Instant::now() + request.timeout;
    let max_turns = request.max_turns.max(1);
    let tools = tools();
    let mut messages = vec![
        ChatMessage::system(system_prompt(sandbox.work_dir())),
        ChatMessage::user(request.prompt.clone()),
    ];
    let mut transcript = Transcript::new(on_event);
    let mut last_text = String::new();

    for turn in 1..=max_turns {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timed_out(&request, turn - 1));
        }
        let turn_request = ChatTurnRequest {
            purpose: "agents.run".to_string(),
            project_id: Some(request.project_id),
            provider: Some(GATEWAY_PROVIDER.to_string()),
            messages: messages.clone(),
            tools: tools.clone(),
            model: request.model.clone(),
            ..Default::default()
        };
        let response = tokio::time::timeout(remaining, ai.chat(turn_request))
            .await
            .map_err(|_| timed_out(&request, turn - 1))?
            .map_err(|error| AgentError::AgentModelFailed {
                run_id: request.run_id,
                turn,
                reason: error.to_string(),
            })?;

        let text = response.content.unwrap_or_default();
        if !text.trim().is_empty() {
            transcript
                .push(json!({ "type": "temps.text", "text": text }))
                .await;
            last_text = text.clone();
        }

        if response.tool_calls.is_empty() {
            transcript
                .push(json!({ "type": "temps.finish", "reason": "stop", "turns": turn }))
                .await;
            return Ok(AgentLoopOutcome {
                transcript: transcript.into_text(),
                final_text: last_text,
                turns: turn,
                exhausted_turns: false,
            });
        }

        let mut assistant = ChatMessage::assistant(text);
        assistant.tool_calls = Some(response.tool_calls.clone());
        messages.push(assistant);
        for call in response.tool_calls {
            transcript
                .push(json!({
                    "type": "temps.tool_call",
                    "id": call.id,
                    "name": call.name,
                    "input": serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .unwrap_or_else(|_| json!({ "raw": call.arguments })),
                }))
                .await;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (output, is_error) = match execute_tool(sandbox, &call, remaining).await {
                Ok(output) => (output, false),
                Err(message) => (message, true),
            };
            let output = bound(&output, MAX_TOOL_OUTPUT_CHARS);
            transcript
                .push(json!({
                    "type": "temps.tool_result",
                    "id": call.id,
                    "output": output,
                    "is_error": is_error,
                }))
                .await;
            messages.push(ChatMessage::tool(call.id, output));
        }
    }

    transcript
        .push(json!({ "type": "temps.finish", "reason": "max_turns", "turns": max_turns }))
        .await;
    Ok(AgentLoopOutcome {
        transcript: transcript.into_text(),
        final_text: last_text,
        turns: max_turns,
        exhausted_turns: true,
    })
}

fn timed_out(request: &AgentLoopRequest, turns: u32) -> AgentError {
    AgentError::AgentTimedOut {
        run_id: request.run_id,
        timeout_secs: request.timeout.as_secs(),
        turns,
    }
}

/// The run's transcript, kept and streamed line by line.
struct Transcript {
    lines: Vec<String>,
    on_event: Option<OnEventCallback>,
}

impl Transcript {
    fn new(on_event: Option<OnEventCallback>) -> Self {
        Self {
            lines: Vec::new(),
            on_event,
        }
    }

    async fn push(&mut self, event: serde_json::Value) {
        let line = event.to_string();
        if let Some(on_event) = &self.on_event {
            on_event(line.clone()).await;
        }
        self.lines.push(line);
    }

    fn into_text(self) -> String {
        self.lines.join("\n")
    }
}

#[derive(Deserialize)]
struct ExecArgs {
    command: String,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    executable: bool,
}

/// Run one tool call. `Err` is a message for the model, not a run failure.
async fn execute_tool(
    sandbox: &dyn RunSandbox,
    call: &ToolCall,
    remaining: Duration,
) -> Result<String, String> {
    match call.name.as_str() {
        SANDBOX_EXEC_TOOL => {
            let args: ExecArgs = parse_args(call)?;
            let secs = args
                .timeout_secs
                .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
                .clamp(1, MAX_COMMAND_TIMEOUT_SECS)
                .min(remaining.as_secs().max(1));
            let cwd = args
                .cwd
                .map(|cwd| resolve(sandbox.work_dir(), &cwd))
                .unwrap_or_else(|| sandbox.work_dir().to_string());
            let cmd = exec_command(&cwd, &args.command, secs);
            let wait = Duration::from_secs(secs) + COMMAND_GRACE;
            let result = tokio::time::timeout(wait, sandbox.exec(cmd))
                .await
                .map_err(|_| format!("The command did not finish within {secs} seconds"))?
                .map_err(|error| format!("The command could not run: {error}"))?;
            Ok(describe_exec(&result, secs))
        }
        SANDBOX_READ_TOOL => {
            let args: ReadArgs = parse_args(call)?;
            let path = resolve(sandbox.work_dir(), &args.path);
            let bytes = sandbox
                .read_file(&path)
                .await
                .map_err(|error| format!("Could not read {path}: {error}"))?;
            let text =
                String::from_utf8(bytes).map_err(|_| format!("{path} is not a text file"))?;
            Ok(read_window(
                &path,
                &text,
                args.offset.unwrap_or(1),
                args.limit.unwrap_or(DEFAULT_READ_LINES),
            ))
        }
        SANDBOX_WRITE_TOOL => {
            let args: WriteArgs = parse_args(call)?;
            if args.content.len() > MAX_WRITE_BYTES {
                return Err(format!(
                    "{} is {} bytes; write at most {MAX_WRITE_BYTES} bytes per call",
                    args.path,
                    args.content.len()
                ));
            }
            let path = resolve(sandbox.work_dir(), &args.path);
            let mode = if args.executable { 0o755 } else { 0o644 };
            sandbox
                .write_file(&path, args.content.as_bytes(), mode)
                .await
                .map_err(|error| format!("Could not write {path}: {error}"))?;
            Ok(format!("Wrote {} bytes to {path}", args.content.len()))
        }
        other => Err(format!(
            "There is no tool named `{other}`. Use {SANDBOX_EXEC_TOOL}, {SANDBOX_READ_TOOL} or \
             {SANDBOX_WRITE_TOOL}."
        )),
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, String> {
    serde_json::from_str(&call.arguments)
        .map_err(|error| format!("The arguments for `{}` are not valid: {error}", call.name))
}

/// `path` against `work_dir` when it is relative.
fn resolve(work_dir: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        let path = path.trim_start_matches("./");
        format!("{}/{}", work_dir.trim_end_matches('/'), path)
    }
}

/// The argv for a shell command run in `cwd`, killed after `secs` by the
/// sandbox's own `timeout`, so it does not outlive the tool call.
fn exec_command(cwd: &str, command: &str, secs: u64) -> Vec<String> {
    vec![
        "timeout".to_string(),
        "-k".to_string(),
        "5".to_string(),
        secs.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        format!("cd {} && {}", shell_quote(cwd), command),
    ]
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn describe_exec(result: &SandboxExecResult, secs: u64) -> String {
    let mut out = if result.exit_code == 124 {
        format!("exit code 124 (stopped after {secs} seconds)")
    } else {
        format!("exit code {}", result.exit_code)
    };
    if !result.stdout.is_empty() {
        out.push_str("\nstdout:\n");
        out.push_str(&result.stdout);
    }
    if !result.stderr.is_empty() {
        out.push_str("\nstderr:\n");
        out.push_str(&result.stderr);
    }
    out
}

/// Lines `offset..offset+limit` of a file, with a note on where to go next.
fn read_window(path: &str, text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.max(1);
    let limit = limit.clamp(1, MAX_READ_LINES);
    if total == 0 {
        return format!("[{path} is empty]");
    }
    if start > total {
        return format!("[{path} has {total} lines; offset {start} is past the end]");
    }
    let last = (start + limit - 1).min(total);
    let mut out = lines[start - 1..last].join("\n");
    if start > 1 || last < total {
        out.push_str(&format!("\n[lines {start}-{last} of {total}"));
        if last < total {
            out.push_str(&format!("; continue with offset={}", last + 1));
        }
        out.push(']');
    }
    out
}

/// At most `max` characters: the head and the tail, with what was left out.
fn bound(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(count - half).collect();
    format!(
        "{head}\n[… {} characters left out …]\n{tail}",
        count - 2 * half
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use temps_ai::{AiError, AiRequest, AiResponse, ChatTurnResponse, TokenStream};

    /// A gateway that answers with scripted turns and records the requests.
    struct ScriptedAi {
        turns: Mutex<VecDeque<Result<ChatTurnResponse, AiError>>>,
        requests: Mutex<Vec<ChatTurnRequest>>,
    }

    impl ScriptedAi {
        fn new(turns: Vec<Result<ChatTurnResponse, AiError>>) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AiService for ScriptedAi {
        async fn is_available(&self) -> bool {
            true
        }
        async fn complete(&self, _request: AiRequest) -> Result<AiResponse, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn chat_stream(&self, _request: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }
        async fn chat(&self, request: ChatTurnRequest) -> Result<ChatTurnResponse, AiError> {
            self.requests.lock().unwrap().push(request);
            self.turns
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(text_turn("out of script")))
        }
    }

    /// A sandbox of files in memory; `exec` records the argv and echoes.
    #[derive(Default)]
    struct MemorySandbox {
        files: Mutex<HashMap<String, Vec<u8>>>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl RunSandbox for MemorySandbox {
        fn work_dir(&self) -> &str {
            "/workspace"
        }
        async fn exec(&self, cmd: Vec<String>) -> Result<SandboxExecResult, AgentError> {
            self.commands.lock().unwrap().push(cmd);
            Ok(SandboxExecResult {
                exit_code: 1,
                stdout: "1 failed".to_string(),
                stderr: "boom".to_string(),
            })
        }
        async fn read_file(&self, path: &str) -> Result<Vec<u8>, AgentError> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| AgentError::Validation {
                    message: format!("{path} does not exist"),
                })
        }
        async fn write_file(
            &self,
            path: &str,
            contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_string(), contents.to_vec());
            Ok(())
        }
    }

    fn text_turn(text: &str) -> ChatTurnResponse {
        ChatTurnResponse {
            content: Some(text.to_string()),
            tool_calls: Vec::new(),
        }
    }

    fn tool_turn(id: &str, name: &str, arguments: serde_json::Value) -> ChatTurnResponse {
        ChatTurnResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            }],
        }
    }

    fn request(max_turns: u32) -> AgentLoopRequest {
        AgentLoopRequest {
            run_id: 7,
            project_id: 3,
            model: Some("gpt-test".to_string()),
            prompt: "Fix the failing test".to_string(),
            max_turns,
            timeout: Duration::from_secs(900),
        }
    }

    #[tokio::test]
    async fn the_agent_works_in_the_sandbox_through_the_gateway_until_it_answers() {
        let ai = ScriptedAi::new(vec![
            Ok(tool_turn(
                "c1",
                SANDBOX_WRITE_TOOL,
                json!({ "path": "src/fix.rs", "content": "fn fixed() {}" }),
            )),
            Ok(tool_turn(
                "c2",
                SANDBOX_EXEC_TOOL,
                json!({ "command": "cargo test" }),
            )),
            Ok(text_turn("Fixed it. <pr_title>fix: the test</pr_title>")),
        ]);
        let sandbox = MemorySandbox::default();

        let outcome = run_agent_loop(&ai, &sandbox, request(10), None)
            .await
            .unwrap();

        assert_eq!(outcome.turns, 3);
        assert!(!outcome.exhausted_turns);
        assert_eq!(
            outcome.final_text,
            "Fixed it. <pr_title>fix: the test</pr_title>"
        );
        assert_eq!(
            sandbox
                .files
                .lock()
                .unwrap()
                .get("/workspace/src/fix.rs")
                .unwrap(),
            b"fn fixed() {}"
        );
        let commands = sandbox.commands.lock().unwrap();
        assert_eq!(
            commands[0],
            [
                "timeout",
                "-k",
                "5",
                "120",
                "sh",
                "-c",
                "cd '/workspace' && cargo test"
            ]
        );

        let requests = ai.requests.lock().unwrap();
        assert!(requests
            .iter()
            .all(|r| r.provider.as_deref() == Some("gateway")));
        assert!(requests
            .iter()
            .all(|r| r.model.as_deref() == Some("gpt-test")));
        assert!(requests.iter().all(|r| r.project_id == Some(3)));
        assert_eq!(requests[0].tools.len(), 3);
        // The last turn saw the command's result, non-zero exit included.
        let last = requests[2].messages.last().unwrap();
        assert_eq!(last.role, "tool");
        assert_eq!(last.tool_call_id.as_deref(), Some("c2"));
        assert!(last
            .content
            .starts_with("exit code 1\nstdout:\n1 failed\nstderr:\nboom"));

        let types: Vec<String> = outcome
            .transcript
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            types,
            [
                "temps.tool_call",
                "temps.tool_result",
                "temps.tool_call",
                "temps.tool_result",
                "temps.text",
                "temps.finish"
            ]
        );
    }

    #[tokio::test]
    async fn the_turn_cap_ends_the_run_and_says_so() {
        let ai = ScriptedAi::new(vec![
            Ok(tool_turn(
                "c1",
                SANDBOX_EXEC_TOOL,
                json!({ "command": "ls" }),
            )),
            Ok(tool_turn(
                "c2",
                SANDBOX_EXEC_TOOL,
                json!({ "command": "ls" }),
            )),
        ]);
        let outcome = run_agent_loop(&ai, &MemorySandbox::default(), request(2), None)
            .await
            .unwrap();
        assert!(outcome.exhausted_turns);
        assert_eq!(outcome.turns, 2);
        let finish: serde_json::Value =
            serde_json::from_str(outcome.transcript.lines().last().unwrap()).unwrap();
        assert_eq!(
            finish,
            json!({ "type": "temps.finish", "reason": "max_turns", "turns": 2 })
        );
    }

    #[tokio::test]
    async fn a_failed_gateway_turn_fails_the_run_with_its_reason() {
        let ai = ScriptedAi::new(vec![Err(AiError::NotAvailable)]);
        let error = run_agent_loop(&ai, &MemorySandbox::default(), request(5), None)
            .await
            .unwrap_err();
        match error {
            AgentError::AgentModelFailed { run_id, turn, .. } => {
                assert_eq!((run_id, turn), (7, 1));
            }
            other => panic!("expected AgentModelFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_bad_tool_call_is_told_to_the_model_not_fatal() {
        let ai = ScriptedAi::new(vec![
            Ok(tool_turn("c1", "deploy", json!({}))),
            Ok(tool_turn(
                "c2",
                SANDBOX_READ_TOOL,
                json!({ "path": "missing.txt" }),
            )),
            Ok(tool_turn("c3", SANDBOX_EXEC_TOOL, json!({ "cmd": "ls" }))),
            Ok(text_turn("done")),
        ]);
        let outcome = run_agent_loop(&ai, &MemorySandbox::default(), request(10), None)
            .await
            .unwrap();
        assert_eq!(outcome.final_text, "done");
        let requests = ai.requests.lock().unwrap();
        let answer = |n: usize| requests[n].messages.last().unwrap().content.clone();
        assert!(answer(1).starts_with("There is no tool named `deploy`"));
        assert!(answer(2).starts_with("Could not read /workspace/missing.txt"));
        assert!(answer(3).starts_with("The arguments for `sandbox_exec` are not valid"));
        let errors = outcome
            .transcript
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|event| event["is_error"] == json!(true))
            .count();
        assert_eq!(errors, 3);
    }

    #[tokio::test]
    async fn every_transcript_line_streams_to_the_run_log() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let on_event: OnEventCallback = Arc::new(move |line: String| {
            let sink = sink.clone();
            Box::pin(async move { sink.lock().unwrap().push(line) })
        });
        let ai = ScriptedAi::new(vec![Ok(text_turn("nothing to do"))]);
        let outcome = run_agent_loop(&ai, &MemorySandbox::default(), request(3), Some(on_event))
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().join("\n"), outcome.transcript);
    }

    #[test]
    fn paths_resolve_against_the_work_dir() {
        assert_eq!(resolve("/workspace", "a/b.rs"), "/workspace/a/b.rs");
        assert_eq!(resolve("/workspace/", "./a.rs"), "/workspace/a.rs");
        assert_eq!(resolve("/workspace", "/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn a_cwd_with_a_quote_cannot_break_out_of_the_cd() {
        let cmd = exec_command("/tmp/it's", "ls", 5);
        assert_eq!(cmd[6], r"cd '/tmp/it'\''s' && ls");
    }

    #[test]
    fn reading_a_window_says_where_to_continue() {
        let text = (1..=10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            read_window("f", &text, 3, 2),
            "3\n4\n[lines 3-4 of 10; continue with offset=5]"
        );
        assert_eq!(read_window("f", &text, 1, 500), text);
        assert_eq!(
            read_window("f", &text, 11, 5),
            "[f has 10 lines; offset 11 is past the end]"
        );
        assert_eq!(read_window("f", "", 1, 5), "[f is empty]");
    }

    #[test]
    fn long_output_keeps_its_head_and_tail() {
        let text = format!("{}{}", "a".repeat(30), "z".repeat(30));
        let bounded = bound(&text, 20);
        assert!(bounded.starts_with(&"a".repeat(10)));
        assert!(bounded.ends_with(&"z".repeat(10)));
        assert!(bounded.contains("40 characters left out"));
        assert_eq!(bound("short", 20), "short");
    }
}
