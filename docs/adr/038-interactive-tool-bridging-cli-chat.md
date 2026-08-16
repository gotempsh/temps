# ADR-038: Interactive Tool Bridging for Subscription-CLI AI Chat

**Status:** Accepted (original limitation superseded by the implementation addendum below)
**Date:** 2026-08-10
**Author:** David Viejo

## Implementation addendum (2026-08-11)

The original decision below documented the limitation that existed before the
interactive transport and scoped tool bridge were implemented. That limitation
no longer describes the product.

Temps now keeps Claude's bidirectional permission/question/plan protocol in
process and exposes the conversation's existing virtual CLI tools to Claude
Code, Codex, and OpenCode through a per-turn MCP endpoint. The endpoint:

- is an in-process Rust HTTP service bound only to `127.0.0.1` on an ephemeral
  port and secret path;
- requires a random bearer token and is destroyed when the provider turn ends;
- delegates to the same project- and user-scoped `temps`/`temps_write`
  dispatchers used by gateway chat;
- keeps writes proposal-only and subject to the existing explicit confirmation
  flow; and
- is registered through each provider CLI's native MCP configuration, with no
  Node.js runtime, package runner, generated executable, or persistent sidecar.

The provider harness selected by the user (`claude`, `codex`, or `opencode`)
must still be installed and authenticated. Only the bridge itself is binary-free.

Sections below are retained as the historical problem statement and alternatives
that led to the implemented design. Statements that interactive tools are not
implemented or that an MCP server is off the roadmap are superseded by this
addendum.

> **Numbering note:** ADR-037 (subscription agent CLI providers) is the most
> recent committed ADR. Numbers 030–036 are occupied by in-flight or parked
> proposals in separate worktrees. 038 is the next free number from the global
> perspective.

## Context

### The trigger

A user of Temps AI chat, with a Claude Code subscription configured as the
active provider (no BYOK key), sent: *"ask me a question using the tool
AskUserQuestion."* Claude Code's model attempted to invoke the `AskUserQuestion`
native tool. The turn ended with an empty response bubble. No error was surfaced.
The question was silently dropped.

### Why it fails today

ADR-037 established `AgentCliAiService::chat_stream()` as the path for
subscription-CLI chat. That implementation invokes:

```
claude --print <prompt> --output-format stream-json
       --dangerously-skip-permissions --verbose
```

Three properties of this invocation make interactive tools impossible:

1. **`--print` is one-shot.** The process has already received the full prompt
   before spawning; there is no stdin channel open after `Command::spawn()`. The
   child writes NDJSON to stdout, then exits. There is nowhere to send a response.

2. **`--dangerously-skip-permissions` pre-approves all tool calls.** When the CLI
   reaches a tool requiring user review (including `AskUserQuestion`,
   `ExitPlanMode`, or any Bash/Write/Edit approval), it auto-approves and
   continues rather than pausing for input. The permission gate that would
   ordinarily surface a prompt to the user is bypassed entirely.

3. **`extract_assistant_text` drops `tool_use` events.** The NDJSON parser in
   `claude.rs` deliberately discards `type=="tool_use"` blocks to avoid leaking
   wire-protocol JSON into the chat UI. This was the correct call for the
   committed scope (tool-less text completions), but it means a `AskUserQuestion`
   event is silently consumed with no trace visible to the user.

### What the Claude Code CLI actually emits

When the model invokes `AskUserQuestion`, the Claude Code CLI in `stream-json`
mode emits an event of the form:

```json
{"type":"tool_use","id":"toolu_...","name":"AskUserQuestion",
 "input":{"questions":[{"question":"What should I focus on?","uuid":"..."}]}}
```

This event is followed by a pause in the subprocess waiting for a response via
stdin, unless `--dangerously-skip-permissions` is active (in which case it is
auto-approved and execution continues without waiting). There is no documented,
stable stdin protocol for feeding a permission response from outside the process.

### Why the Claude Agent SDK does not directly apply

Paseo (the reference implementation researched for ADR-037) handles this by
using the `@anthropic-ai/claude-agent-sdk` (TypeScript/Node). The SDK's
`query()` API accepts a `canUseTool: CanUseTool` callback. When the spawned
`claude` process requests a tool, the SDK calls that callback in-process, and
the caller can `await new Promise(...)` — keeping the process alive — until
the user responds. The SDK manages the underlying subprocess stdin/stdout
choreography invisibly.

That SDK is TypeScript/Node-only. Anthropic has not published a Rust equivalent.
Temps is a Rust backend that invokes CLIs as raw OS subprocesses. There is no
in-process callback mechanism available in Rust today.

### What is already confirmed from ADR-037 research

- Anthropic closed ACP (Agent Client Protocol, stdio-based permission callbacks)
  as NOT_PLANNED in February 2026 (issue #6686).
- `--permission-prompt-tool` MCP flag exists in Claude Code but has no documented
  working reference implementation as of this writing (issue #1175, open).
- Codex has `codex app-server` (JSON-RPC-over-stdio server mode) with
  `requestApproval` RPCs for tool permissions. This is a separate, documented
  mode.
- OpenCode's interactive permission story is entirely unresearched. Its protocol
  is unknown.
- `max_turns: 1` is the current cap in `AgentCliAiService`. At one turn, a
  full agentic loop with plan-then-approve (`ExitPlanMode`) cannot complete
  anyway — `ExitPlanMode` only fires at the end of a planning turn before
  the model executes its plan across subsequent turns.

### The current `TokenStream` and SSE shape

`TokenStream = Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>>` —
plain text strings only. The SSE handler in `temps-ai-chat` emits `data:` frames
containing raw text chunks. There is no structured event envelope, no
`event: permission_requested` SSE type, and no frontend component for rendering
a permission request or collecting a response. Adding interactive tool bridging
would require extending all three layers: the stream type, the SSE handler, and
the frontend.

### Forces

- **Single-binary self-hosted model.** Operators run one binary on a Hetzner
  cpx22 (3 vCPU / 4 GB RAM). Adding a persistent Node.js sidecar process is a
  non-trivial operational burden: another runtime to install, health-check,
  restart policy to define, and package in the distribution. The managed Temps
  Cloud path is less constrained, but the ADR must be valid for self-hosted.
- **Three providers, one is partially understood.** Only Claude Code has been
  researched. Codex has a documented JSON-RPC server mode. OpenCode is entirely
  unknown. Any design that cannot degrade gracefully to "not supported on this
  provider" would break today when OpenCode is active.
- **The existing guards in ADR-037 are load-bearing.** `chat_stream_turn()`
  returns `NotAvailable` unconditionally. `chat_stream()` rejects non-empty tool
  lists. These guards keep `--dangerously-skip-permissions` safe. Relaxing them
  without first implementing a real permission bridge would expose native CLI
  tools (`bash`, file editing) to auto-approved execution on the Temps host.
- **No MCP server on the product roadmap.** The `--permission-prompt-tool` MCP
  flag would require Temps to run an MCP server as the permission endpoint. MCP
  server is not on the roadmap (project memory: "No MCP ever"). Even if the flag
  works, this constraint eliminates it as a near-term option.
- **`AskUserQuestion` and `ExitPlanMode` are agentic-mode tools, not chat tools.**
  These tools fire when Claude Code is running an autonomous agent loop across
  multiple turns (planning, executing, reviewing). The current CLI chat path is
  deliberately `max_turns: 1` — a conversational exchange, not an agentic loop.
  The user trigger ("ask me a question using the tool AskUserQuestion") is an
  atypical prompt that forces the model into a tool invocation in a context where
  the infrastructure cannot support it.

## Decision

### Recommendation: Option C — explicit, documented limitation with honest surfacing

Interactive tool bridging (AskUserQuestion, ExitPlanMode, generic tool
permission approval) is **not implemented** in this phase. The CLI-backed chat
path continues to be scoped to tool-less, single-turn conversational exchange
exactly as ADR-037 established.

The gap is treated as a **discoverable, honestly surfaced limitation** rather
than a silent failure. Specifically:

1. **User-visible indicator in the chat UI.** When the active AI provider is a
   subscription CLI (not a BYOK gateway), the chat composer shows a persistent,
   non-dismissable callout: *"Claude Code chat is in conversational mode.
   Interactive tools (plan approval, mid-turn questions) are not supported —
   the model will not be able to pause and ask you a question. For full
   interactive capability, configure a BYOK API key."* This surfaces at the
   point of use, not buried in settings.

2. **Structured `tool_use` drop log.** When `extract_assistant_text` discards a
   `tool_use` event, the NDJSON parser logs at `warn!` level with the tool name
   and turn ID. The user never sees wire-protocol JSON, but operators can
   diagnose via logs why a turn ended unexpectedly empty. Current behavior: the
   event is silently consumed with no trace.

3. **Capability endpoint extended.** `GET /api/ai/provider-status` (defined in
   ADR-037 §7) adds a `supports_interactive_tools: bool` field. When the active
   provider is a CLI, this is `false`. The frontend reads this flag to decide
   whether to show the callout. New field shape:

   ```rust
   pub struct AiProviderStatusResponse {
       // ... existing fields from ADR-037 ...
       /// Whether this provider path supports mid-turn interactive tools
       /// (AskUserQuestion, ExitPlanMode, tool permission prompts).
       /// Always false for agent-CLI providers; always true for BYOK gateway.
       pub supports_interactive_tools: bool,
   }
   ```

4. **`max_turns` kept at 1 for `chat_stream()`.** This is both a scope
   constraint and a partial mitigation: at one turn, `ExitPlanMode` cannot fire
   at all (it fires at the end of a planning turn, before subsequent execution
   turns). `AskUserQuestion` still can fire within a single turn, but it will be
   auto-approved by `--dangerously-skip-permissions` and its content discarded.
   The user-visible callout covers this case.

### Options evaluated and rejected

#### Option A: Reverse-engineer the Claude Code CLI's stdin permission protocol

**What it would take.** Drop `--print` and `--dangerously-skip-permissions`.
Run the CLI in a long-lived interactive mode. Detect `tool_use` events in stdout.
For each permission-gated event, push a structured `permission_requested` SSE
event to the client, collect the user's response via a REST endpoint, and write
a response frame to the child's stdin. Implement a pending-request state machine
(keyed by request UUID) across the HTTP request boundary.

**Why rejected.**

- **The stdin protocol is undocumented and known-unstable.** Anthropic's own ACP
  issue (#6686) was closed NOT_PLANNED because they explicitly chose not to
  standardize a machine-readable stdio permission protocol. Implementing against
  an undocumented internal protocol is a maintenance liability that breaks on
  any `claude` CLI release.
- **Provider-specific and non-portable.** Codex's protocol is `codex app-server`
  JSON-RPC (different wire format, different spawn flags). OpenCode's protocol
  is unknown. Implementing per-provider protocol handlers for all three creates
  three independent reverse-engineered integrations with no test surface.
- **Not just an implementation cost.** Keeping a child process alive across an
  HTTP request cycle (the pending-request state machine) adds new failure modes:
  what happens if the user closes the browser tab mid-permission? What is the
  timeout policy for a pending `AskUserQuestion`? What if the CLI process exits
  while a permission is pending? These failure modes are real and would need to
  be fully specified and implemented — this is a significant new subsystem.
- **`--permission-prompt-tool` is the documented alternative, but it requires
  running an MCP server.** This is explicitly off the roadmap.

**When to reconsider.** If Anthropic publishes a stable, machine-readable stdin
permission protocol for headless `claude` invocations (via a future SDK or a
revised ACP design), Option A becomes viable for Claude Code specifically. It
would still need separate solutions for Codex and OpenCode.

#### Option B: Node.js sidecar using the official Claude Agent SDK

**What it would take.** A small Node.js/TypeScript sidecar process that uses
`@anthropic-ai/claude-agent-sdk`. The sidecar accepts a prompt over a local Unix
socket or HTTP loopback, calls `query()` with a `canUseTool` callback, and
forwards permission requests back to the Rust backend over the same channel.
The Rust backend implements the state machine, SSE events, and response API.
This is architecturally equivalent to what paseo does as a standalone daemon.

**Why not adopted in this ADR.**

- **Operational cost is high for self-hosted.** A Node.js runtime must be present
  on the Temps host. The single-binary self-hosted story breaks: operators must
  now install Node.js in addition to the `temps` binary. The `deploy.sh` script
  would need to provision it. The sidecar needs a restart policy, a
  health-check, a log stream, and process supervision. On a cpx22 with 4 GB RAM,
  this is non-trivial resource competition.
- **Only covers Claude Code for now.** The SDK is TypeScript/Node only and covers
  the Claude Code CLI. Codex has its own `codex app-server` JSON-RPC mode (also
  TypeScript). OpenCode is unknown. A sidecar that only serves Claude Code is a
  partial solution with a confusing asymmetry: interactive tools work when Claude
  Code is the provider but silently fail on Codex or OpenCode.
- **Managed Temps Cloud could absorb the cost.** On Cloud, the sidecar could run
  as a separate container. This changes the calculus and makes Option B viable
  as a Cloud-specific premium feature, not a universal change to the self-hosted
  architecture.

**When to reconsider.** If interactive tool bridging becomes a demonstrated
user demand (measured, not assumed), Option B is the correct long-term path for
Claude Code support. The design should be: sidecar as a separate deployable
component, enabled only when opted into, with explicit documentation of the
Node.js runtime dependency. Self-hosted operators who cannot run Node.js get the
Option C behavior (documented limitation + capability flag). The state machine
design below (§ Implementation Notes) applies regardless of which transport layer
implements it.

#### Option D: Partial support — ExitPlanMode only via re-invocation

**What it would take.** For `ExitPlanMode` specifically (plan review before
execution), a simpler mechanism: when the model outputs a `tool_use` block with
`name: "ExitPlanMode"` and `input.plan`, extract the plan text, emit it as a
special SSE event to the frontend (a non-text `plan_review` event), let the user
approve or reject via a separate REST endpoint, then re-invoke the CLI with the
full conversation history plus a synthetic "user approved the plan" turn. This
avoids any stdin protocol complexity.

**Why rejected over Option C.**

- `max_turns: 1` means `ExitPlanMode` cannot fire in the current architecture.
  Raising `max_turns` to allow multi-turn agentic execution would require
  re-evaluating the security posture of `--dangerously-skip-permissions` for
  multi-turn runs — that is a separate design effort, not just adding a
  `ExitPlanMode` handler.
- Re-invocation loses the subprocess's in-memory state (tool results, partial
  execution). Claude Code's `--continue` flag resumes from disk state, but the
  re-invocation approach works only if the prior turn's state was fully committed
  to Claude Code's local session store, which is not guaranteed for a synthetic
  mid-plan pause.
- Adds frontend complexity (a new SSE event type, a new UI card, a new REST
  endpoint) for a feature that does not work yet at the infrastructure level
  without also raising `max_turns`. Building UI for a broken feature is the
  wrong order.

## Consequences

### Positive

- No new subsystem, no new failure modes, no new operational dependencies.
- The limitation is honest and surfaced at the point of use, not discovered by
  users when a turn silently produces an empty bubble.
- `warn!` logging on dropped `tool_use` events makes the failure diagnosable
  from server logs without exposing protocol JSON to the user.
- The `supports_interactive_tools` capability flag gives the frontend a clean,
  typed boolean to render the appropriate UI rather than inferring provider type
  from configuration.
- The ADR records the design space clearly for the next person who picks this up,
  so they do not re-litigate the Option A and Option B evaluation.

### Negative

- Users with Claude Code subscriptions who want to use `AskUserQuestion`,
  plan-review mode, or any permission-gated tool in chat must use a BYOK API
  key and the gateway path. There is no migration path to interactive tools on
  the subscription-CLI chat path in this phase.
- The user-visible callout adds friction for the majority of subscription-CLI
  chat users who are doing plain conversational exchange and will never trigger
  these tools. The callout must be concise and not alarming — it describes a
  missing capability, not an error.

### Risks

- **User expectation mismatch.** Claude Code's own UI supports interactive tools
  in agentic mode. Users who configure their Claude Code subscription in Temps
  and then hit the limitation may experience it as a regression from their local
  Claude Code experience. The callout mitigates this if it is visible before the
  user's first message.
- **`max_turns` creep.** If a future change raises `max_turns` above 1 for
  CLI-chat (e.g. to support multi-turn conversational context), `ExitPlanMode`
  and `AskUserQuestion` will be triggerable in practice, making the current
  silent-drop behavior worse (the model runs an agentic loop, pauses for a
  question, gets silently auto-approved, and the user sees a completed action
  they did not authorize). This risk is blocked as long as `max_turns: 1` holds.
  Any change to `max_turns` must revisit this ADR before shipping.

## Open Questions

1. **Is `claude`'s stdin format for permission responses stable or documented
   anywhere?** The ACP issue (#6686) was closed NOT_PLANNED, but that does not
   mean the CLI has no stdin protocol — it may have an undocumented one that
   tools like paseo exploit indirectly through the SDK. Confirming whether the
   SDK's IPC with the spawned process is public/stable would change the Option A
   assessment.

2. **Codex `codex app-server` JSON-RPC mode: does it support
   `AskUserQuestion`-equivalent user-question RPCs, or only bash/file
   approval?** ADR-037 confirmed `requestApproval` RPCs for tool permissions.
   Whether Codex has a user-question primitive in that mode needs verification
   before designing a unified permission protocol across providers.

3. **OpenCode interactive/permission story: entirely unknown.** Before any Phase
   2 design can claim provider parity, OpenCode's stdin protocol (or lack of
   one) must be researched.

4. **`TokenStream` extensibility.** The current `TokenStream` type is
   `Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>>` — plain strings.
   A real interactive-tool bridge would need structured events in the stream
   (a variant enum alongside `Text(String)`, similar to the existing
   `ChatStreamDelta` that `chat_stream_turn` uses). Changing `TokenStream` to
   a structured enum is a breaking change to all `chat_stream()` callers. This
   needs a migration design before Option A or B could be implemented.

5. **Frontend message/event shape for permission prompts.** The SSE handler in
   `temps-ai-chat/src/handlers.rs` currently sends plain-text `data:` frames.
   Adding a `permission_requested` SSE event type requires a named event
   (`event: permission_requested\ndata: {...}`) and a new frontend component for
   rendering it and collecting the user's response. The design of that component
   (approval buttons, question text input, timeout indicator) is unspecified.

6. **Session persistence across a permission response.** If interactive tool
   bridging were ever implemented, what happens when the user responds to a
   permission prompt after the original HTTP request has completed? The current
   SSE model is one HTTP connection per turn. The permission state machine must
   outlive that connection. This requires either WebSocket semantics or a named
   session ID that associates the response with the pending subprocess.

## Implementation Notes

### Phase 1 (this ADR) — Honest surfacing, no new subsystem

**Affected crates:**
- `crates/temps-agents/src/ai_cli/claude.rs` — add `warn!` logging when a
  `tool_use` event is dropped by `extract_assistant_text`. Log the tool name and
  the turn's session ID if available. Do not log the full `input` object
  (may contain user data or credentials passed to the tool).
- `crates/temps-ai-chat/src/handlers.rs` — extend `AiProviderStatusResponse`
  with `supports_interactive_tools: bool`. Set to `false` when active provider
  is agent-CLI, `true` when it is BYOK gateway.
- `web/src/` — add a non-dismissable callout component in the AI chat composer
  area, visible when `supports_interactive_tools` is `false`. Copy: *"Claude
  Code chat runs in conversational mode. Interactive tools (mid-turn questions,
  plan approval) are not available here — [Configure a BYOK API key](/ai-gateway)
  for full capability."* Use the existing onboarding-state pattern from ADR-037.

**Migration:** None. This is additive.
**Breaking changes:** None. `supports_interactive_tools` is a new field on an
existing response type; existing clients that do not read it are unaffected.

### Phase 2 (deferred, requires separate ADR) — Interactive bridging

If interactive tool bridging is implemented in a future phase, the design must
address at minimum:

- **Stream type migration:** `TokenStream` becomes a structured stream of
  `CliChatEvent` (an enum with `Text(String)` and `PermissionRequest(...)`)
  rather than plain `String`. All callers of `chat_stream()` must be updated.
- **State machine:** A `PendingPermissionRegistry` (keyed by turn UUID →
  `oneshot::Sender<PermissionResult>`) persisted for the lifetime of the CLI
  subprocess, independent of the HTTP request lifecycle.
- **Per-provider implementations:** Claude Code first (via Option B sidecar or
  a verified stable stdin protocol), Codex second (via `codex app-server`
  JSON-RPC), OpenCode only after its protocol is confirmed.
- **Security review required** before shipping: `--dangerously-skip-permissions`
  must be removed for any turn path that enables native tool execution. The
  security-auditor agent must sign off on the replacement mechanism.
- **`max_turns` policy:** Multi-turn agentic execution is a prerequisite for
  `ExitPlanMode` support. The security posture of multi-turn CLI execution on
  the Temps host must be re-evaluated in that separate ADR.

## References

- `crates/temps-agents/src/ai_cli/claude.rs` — `run()` method (~line 119),
  `extract_assistant_text` dispatcher, `--dangerously-skip-permissions` usage
- `crates/temps-ai-agent-cli/src/service.rs` — `AgentCliAiService::chat_stream()`,
  the `on_event` callback that currently filters to `extract_assistant_text`
  results only
- `crates/temps-ai-agent-cli/src/dispatch.rs` — `DispatchingAiService`,
  `chat_capable()` hardcoded false for CLI providers
- `crates/temps-ai/src/streaming.rs` — `TokenStream` type definition and
  `ChatStreamDelta` (the structured-event precedent for `chat_stream_turn`)
- ADR-037 — Subscription-backed agent CLI providers (the foundation this ADR
  extends; especially §5 scope constraint, the blanket-bypass rationale, and
  the "Per-tool permission bridge via paseo pattern — considered; deferred"
  alternative)
- ADR-022 — AI foundation and `AiService` trait
- ADR-024 — API-as-LLM-tools (why `chat_stream_turn` cannot delegate to CLIs)
- Claude Code CLI issue #6686 — ACP stdio permission callbacks, closed
  NOT_PLANNED, February 2026
- Claude Code CLI issue #1175 — `--permission-prompt-tool` MCP delegation flag,
  open, no working reference implementation as of this writing
- paseo `getpaseo/paseo` — reference implementation of `canUseTool` SDK callback
  pattern and `codex app-server` JSON-RPC approval routing (Option B archetype)
