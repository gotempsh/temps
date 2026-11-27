// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An agent run on Temps' own tool loop (ADR-116 D2).
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
/// Largest file `sandbox_read_file` reads. The sandbox refuses a larger one
/// before sending it, so a one-line read of a huge generated file never
/// lands in the control plane's memory; the model pages it with
/// `sandbox_exec` (`sed -n`, `head`) instead.
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// The sandbox an agent run works in. [`RegistrySandbox`] is the real one;
/// tests use an in-memory one.
#[async_trait]
pub trait RunSandbox: Send + Sync {
    /// Absolute work directory that relative paths resolve against.
    fn work_dir(&self) -> &str;
    async fn exec(&self, cmd: Vec<String>) -> Result<SandboxExecResult, AgentError>;
    /// The file at `path`, refused when larger than `max_bytes`.
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, AgentError>;
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

    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, AgentError> {
        self.registry
            .read_file_bounded(self.run_id, path, max_bytes)
            .await
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
    /// Most model turns (one gateway call each). 0 sets no cap, as it did
    /// for the coding CLIs: the time budget then bounds the run.
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
    /// Tokens the gateway reported across every turn; `None` when it
    /// reported none.
    pub tokens_input: Option<u64>,
    pub tokens_output: Option<u64>,
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
    let max_turns = request.max_turns;
    let tools = tools();
    let mut messages = vec![
        ChatMessage::system(system_prompt(sandbox.work_dir())),
        ChatMessage::user(request.prompt.clone()),
    ];
    let mut transcript = Transcript::new(on_event);
    let mut last_text = String::new();
    let mut usage = Usage::default();
    let mut turn = 0u32;

    while max_turns == 0 || turn < max_turns {
        turn += 1;
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
        usage.add(response.usage);

        let text = response.content.unwrap_or_default();
        if !text.trim().is_empty() {
            transcript
                .push(json!({ "type": "temps.text", "text": text }))
                .await;
            last_text = text.clone();
        }

        if response.tool_calls.is_empty() {
            // A reply with neither text nor a tool call is no final message:
            // an empty `choices` array or an empty assistant message. Ending
            // the run as a success would hand a report run a finish event
            // for its analysis.
            if text.trim().is_empty() {
                return Err(AgentError::AgentModelFailed {
                    run_id: request.run_id,
                    turn,
                    reason: "the model's reply had no text and no tool call".to_string(),
                });
            }
            transcript
                .push(json!({ "type": "temps.finish", "reason": "stop", "turns": turn }))
                .await;
            return Ok(AgentLoopOutcome {
                transcript: transcript.into_text(),
                final_text: last_text,
                turns: turn,
                exhausted_turns: false,
                tokens_input: usage.input,
                tokens_output: usage.output,
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
            // The deadline is the whole run's, so it bounds every call in the
            // batch: none starts once it has passed, and one that runs into
            // it ends the run as timed out, not as a finished turn.
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timed_out(&request, turn));
            }
            let (output, is_error) = match tokio::time::timeout(
                remaining,
                execute_tool(sandbox, &call, remaining),
            )
            .await
            {
                Ok(Ok(output)) => (output, false),
                Ok(Err(message)) => (message, true),
                Err(_) => return Err(timed_out(&request, turn)),
            };
            if Instant::now() >= deadline {
                return Err(timed_out(&request, turn));
            }
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
        tokens_input: usage.input,
        tokens_output: usage.output,
    })
}

/// The tokens a run's turns used, summed over the turns the gateway
/// reported them for.
#[derive(Default)]
struct Usage {
    input: Option<u64>,
    output: Option<u64>,
}

impl Usage {
    fn add(&mut self, turn: Option<temps_ai::ChatTurnUsage>) {
        if let Some(turn) = turn {
            self.input = Some(self.input.unwrap_or(0).saturating_add(turn.input_tokens));
            self.output = Some(self.output.unwrap_or(0).saturating_add(turn.output_tokens));
        }
    }
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
                .min(remaining.as_secs())
                .max(1);
            let cwd = args
                .cwd
                .map(|cwd| resolve(sandbox.work_dir(), &cwd))
                .unwrap_or_else(|| sandbox.work_dir().to_string());
            let cmd = exec_command(&cwd, &args.command, secs);
            // Never past the run's deadline, which the caller also enforces.
            let wait = (Duration::from_secs(secs) + COMMAND_GRACE).min(remaining);
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
                .read_file(&path, MAX_READ_BYTES)
                .await
                .map_err(|error| {
                    format!(
                        "Could not read {path}: {error}. Files over {MAX_READ_BYTES} bytes are \
                         not read whole; page through one with {SANDBOX_EXEC_TOOL} (sed -n, head)."
                    )
                })?;
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
        // `cd X && a; b` would still run `b` where the shell started when
        // `X` is missing: stop the whole script instead.
        format!("cd {} || exit 1\n{}", shell_quote(cwd), command),
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
    // Counted and sliced lazily: no vector of every line for a one-line read.
    let total = text.lines().count();
    let start = offset.max(1);
    let limit = limit.clamp(1, MAX_READ_LINES);
    if total == 0 {
        return format!("[{path} is empty]");
    }
    if start > total {
        return format!("[{path} has {total} lines; offset {start} is past the end]");
    }
    let last = (start + limit - 1).min(total);
    let mut out = text
        .lines()
        .skip(start - 1)
        .take(last + 1 - start)
        .collect::<Vec<_>>()
        .join("\n");
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
        read_limits: Mutex<Vec<u64>>,
        /// How long each `exec` takes.
        exec_delay: Duration,
    }

    #[async_trait]
    impl RunSandbox for MemorySandbox {
        fn work_dir(&self) -> &str {
            "/workspace"
        }
        async fn exec(&self, cmd: Vec<String>) -> Result<SandboxExecResult, AgentError> {
            self.commands.lock().unwrap().push(cmd);
            tokio::time::sleep(self.exec_delay).await;
            Ok(SandboxExecResult {
                exit_code: 1,
                stdout: "1 failed".to_string(),
                stderr: "boom".to_string(),
            })
        }
        async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, AgentError> {
            self.read_limits.lock().unwrap().push(max_bytes);
            let contents = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| AgentError::Validation {
                    message: format!("{path} does not exist"),
                })?;
            if contents.len() as u64 > max_bytes {
                return Err(AgentError::Validation {
                    message: format!("{path} is larger than {max_bytes} bytes"),
                });
            }
            Ok(contents)
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
            usage: None,
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
            usage: None,
        }
    }

    fn with_usage(mut turn: ChatTurnResponse, input: u64, output: u64) -> ChatTurnResponse {
        turn.usage = Some(temps_ai::ChatTurnUsage {
            input_tokens: input,
            output_tokens: output,
        });
        turn
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
                "cd '/workspace' || exit 1\ncargo test"
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

    /// The run's deadline bounds the whole batch: a call that runs into it
    /// ends the run as timed out, the calls after it never start, and the
    /// last turn does not report a turn cap instead.
    #[tokio::test]
    async fn a_tool_batch_stops_at_the_run_deadline() {
        let ai = ScriptedAi::new(vec![Ok(ChatTurnResponse {
            content: None,
            tool_calls: vec![
                ToolCall {
                    id: "c1".to_string(),
                    name: SANDBOX_EXEC_TOOL.to_string(),
                    arguments: json!({ "command": "sleep 60" }).to_string(),
                },
                ToolCall {
                    id: "c2".to_string(),
                    name: SANDBOX_WRITE_TOOL.to_string(),
                    arguments: json!({ "path": "late.txt", "content": "x" }).to_string(),
                },
            ],
            usage: None,
        })]);
        let sandbox = MemorySandbox {
            exec_delay: Duration::from_secs(5),
            ..Default::default()
        };
        let mut req = request(1);
        req.timeout = Duration::from_millis(300);
        let started = Instant::now();
        let error = run_agent_loop(&ai, &sandbox, req, None).await.unwrap_err();
        assert!(
            matches!(
                error,
                AgentError::AgentTimedOut {
                    run_id: 7,
                    turns: 1,
                    ..
                }
            ),
            "{error:?}"
        );
        // Not the exec's own 1 s floor plus its 15 s grace.
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!sandbox
            .files
            .lock()
            .unwrap()
            .contains_key("/workspace/late.txt"));
    }

    /// A read asks the sandbox for at most MAX_READ_BYTES, so a huge file is
    /// refused there instead of being buffered here, and the model is told
    /// how to page it.
    #[tokio::test]
    async fn a_read_is_bounded_before_the_file_is_buffered() {
        let ai = ScriptedAi::new(vec![
            Ok(tool_turn(
                "c1",
                SANDBOX_READ_TOOL,
                json!({ "path": "big.log", "limit": 1 }),
            )),
            Ok(text_turn("done")),
        ]);
        let sandbox = MemorySandbox::default();
        sandbox.files.lock().unwrap().insert(
            "/workspace/big.log".to_string(),
            vec![b'a'; MAX_READ_BYTES as usize + 1],
        );
        run_agent_loop(&ai, &sandbox, request(3), None)
            .await
            .unwrap();
        assert_eq!(*sandbox.read_limits.lock().unwrap(), vec![MAX_READ_BYTES]);
        let requests = ai.requests.lock().unwrap();
        let result = &requests[1].messages.last().unwrap().content;
        assert!(
            result.contains("Could not read /workspace/big.log"),
            "{result}"
        );
        assert!(result.contains("sed -n"), "{result}");
    }

    #[test]
    fn a_read_window_slices_lines_without_collecting_them_all() {
        let text = "one\ntwo\nthree\nfour\n";
        assert_eq!(
            read_window("/f", text, 2, 2),
            "two\nthree\n[lines 2-3 of 4; continue with offset=4]"
        );
        assert_eq!(read_window("/f", text, 1, 10), "one\ntwo\nthree\nfour");
        assert_eq!(
            read_window("/f", text, 9, 1),
            "[/f has 4 lines; offset 9 is past the end]"
        );
        assert_eq!(read_window("/f", "", 1, 1), "[/f is empty]");
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
        assert_eq!(cmd[6], "cd '/tmp/it'\\''s' || exit 1\nls");
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

    /// A cap of 0 is no cap, as the coding CLIs read it: the run goes on
    /// until the model gives a final message (or the time budget ends).
    #[tokio::test]
    async fn zero_turns_sets_no_cap() {
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
            Ok(text_turn("done")),
        ]);
        let outcome = run_agent_loop(&ai, &MemorySandbox::default(), request(0), None)
            .await
            .unwrap();
        assert_eq!(outcome.turns, 3);
        assert!(!outcome.exhausted_turns);
        assert_eq!(outcome.final_text, "done");
    }

    /// The run sums the tokens each turn reports, and reports none when the
    /// gateway gave none.
    #[tokio::test]
    async fn the_run_reports_the_tokens_its_turns_used() {
        let ai = ScriptedAi::new(vec![
            Ok(with_usage(
                tool_turn("c1", SANDBOX_EXEC_TOOL, json!({ "command": "ls" })),
                1200,
                40,
            )),
            Ok(with_usage(text_turn("done"), 1500, 60)),
        ]);
        let outcome = run_agent_loop(&ai, &MemorySandbox::default(), request(5), None)
            .await
            .unwrap();
        assert_eq!(outcome.tokens_input, Some(2700));
        assert_eq!(outcome.tokens_output, Some(100));

        let silent = ScriptedAi::new(vec![Ok(text_turn("done"))]);
        let outcome = run_agent_loop(&silent, &MemorySandbox::default(), request(5), None)
            .await
            .unwrap();
        assert_eq!((outcome.tokens_input, outcome.tokens_output), (None, None));
    }

    /// A gateway reply with no text and no tool call (an empty `choices`
    /// array) fails the run instead of completing it with nothing.
    #[tokio::test]
    async fn an_empty_reply_fails_the_run() {
        let ai = ScriptedAi::new(vec![Ok(ChatTurnResponse {
            content: None,
            tool_calls: Vec::new(),
            usage: None,
        })]);
        let error = run_agent_loop(&ai, &MemorySandbox::default(), request(5), None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::AgentModelFailed { turn: 1, .. }),
            "{error:?}"
        );

        let blank = ScriptedAi::new(vec![Ok(text_turn("   "))]);
        assert!(
            run_agent_loop(&blank, &MemorySandbox::default(), request(5), None)
                .await
                .is_err()
        );
    }

    /// A missing working directory stops the whole script: nothing after
    /// the first command runs in the directory the shell started in.
    #[test]
    fn a_missing_cwd_stops_every_command_after_it() {
        let dir = std::env::temp_dir().join(format!("agent-loop-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing");
        let marker = dir.join("ran");
        let cmd = exec_command(
            missing.to_str().unwrap(),
            &format!("pwd; touch {}", shell_quote(marker.to_str().unwrap())),
            5,
        );
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd[6])
            .current_dir(&dir)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success());
        assert!(!marker.exists(), "a command after the failed cd ran");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
