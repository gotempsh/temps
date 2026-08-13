# ADR-037: Subscription-Backed Agent CLI Providers for Temps AI Workloads

> **Implementation update (2026-08-11):** The original workload matrix and
> tool-routing restrictions below describe the initial rollout. CLI-backed chat
> now supports the existing scoped `temps` and confirm-gated `temps_write`
> virtual CLI tools through an authenticated, per-turn, in-process loopback MCP
> bridge. Claude retains its interactive permission/question/plan transport;
> Codex and OpenCode use the shared tool-executor loop. No bridge executable or
> sidecar runtime is required, although the selected provider harness binary
> must still be installed and authenticated. ADR-038 records the implementation
> addendum.

> **Architecture update (2026-08-11):** Provider selection and turn execution
> now use provider-neutral contracts. `ProviderCapabilities` is the only model,
> thinking, permission, authentication-source, and realtime capability shape
> consumed by chat and settings. `TurnServices` is the only injection point for
> scoped tools and user interactions. `AiProviderRegistry` routes both gateway
> and CLI adapters through the same conversation loop. Provider-specific code is
> limited to authentication/status discovery, capability discovery, command/API
> invocation, and wire-event parsing.

**Status:** Accepted
**Date:** 2026-08-08
**Author:** David Viejo

## Current architecture

```text
Chat / diagnostics / future AI features
                  │
                  ▼
        temps_ai::AiService
          ├─ capabilities_for(provider, refresh)
          └─ chat_stream_turn_with_services(request, TurnServices)
                  │
                  ▼
          AiProviderRegistry
          ├─ gateway adapter
          └─ CLI adapter registry
               ├─ Claude Code
               ├─ Codex
               ├─ OpenCode
               └─ future adapter
                  │
                  ▼
       one ConversationService turn loop
       persistence · scoped tools · write proposals
       user interactions · streaming · cancellation · errors
```

The extension boundary is deliberately narrow:

1. A provider adapter reports `ProviderCapabilities`, including the exact
   account-visible models and per-model thinking modes.
2. It implements the common `AiService` turn contract, or delegates CLI turns
   through `AiCliProvider::run_turn`.
3. A CLI-backed adapter adds one `ProviderCatalogEntry` containing its factory,
   permission modes, host-access requirement, and realtime flags.

Conversation persistence, project/user authorization, tool registration,
confirm-gated writes, interaction resolution, SSE events, cancellation, and
error handling do not belong to adapters. This prevents a fourth provider from
copying the Claude/Codex/OpenCode integration and drifting from it.

### Security and lifecycle invariants

- Harness subprocesses start from an empty environment and receive only a small
  runtime allowlist, their own provider credential, and the ephemeral MCP token.
- The MCP bridge is loopback-only, bearer-authenticated, scoped to one turn, and
  emits a terminal result for success, failure, and timeout so a call cannot be
  replayed by the fallback dispatcher.
- Provider tasks and pending interaction waiters are owned by the returned
  stream. Stop, disconnect, and timeout cancel the complete turn and remove its
  exact pending approval registration.
- Executable write-proposal parameters are encrypted at rest. Browser-visible
  action data and persisted tool metadata contain only recursively redacted
  display values.

## Context

Temps AI features today route exclusively through the BYOK AI Gateway
(`temps-ai-gateway`). Every AI call — alert summaries, debug chat, build
diagnostics, structured-output extraction — invokes `GatewayAiService`, the
single registered `Arc<dyn temps_ai::AiService>` (registered at
`crates/temps-ai-gateway/src/plugin.rs:54-58`). This requires an operator to
provision, encrypt, rotate, and monitor provider API keys even when they already
pay for a subscription-backed coding agent such as Claude Code or Codex.

**The autofixer already solves this differently.** `temps-agents` has a complete
provider-adapter layer for running AI CLIs:

- **`AiCliProvider` trait** (`crates/temps-agents/src/ai_cli/mod.rs:67-75`):
  `check_installed()`, `get_status()`, `run(AiRunConfig)`,
  `continue_conversation()`.
- **Three implementations**: Claude Code (`ai_cli/claude.rs`), Codex
  (`ai_cli/codex.rs`), OpenCode (`ai_cli/opencode.rs`).
- **Provider catalog** (`crates/temps-agents/src/ai_cli/catalog.rs:114`): a
  static `PROVIDER_CATALOG` array of `ProviderCatalogEntry` values with install
  commands, auth flavors, and model identifiers. Adding a new provider requires
  only a catalog entry and a trait implementation — no DB migrations, no UI
  changes (invariant documented at `catalog.rs:1-9`).
- **Three credential delivery formats** (`CredentialFormat` enum,
  `catalog.rs:16-27`): `ApiKey` (env var), `OauthToken` (JSON credential file,
  e.g. `~/.claude/.credentials.json`), `ConfigFile` (arbitrary body, e.g.
  OpenCode's `auth.json`).
- **Three-tier key resolution** (`executor.rs:2161-2197`): per-agent encrypted
  key → shared `ai_provider_keys` row → empty string (CLI uses subscription
  mode via ambient credential, e.g. `~/.claude`).
- **Sandbox execution path** (`executor.rs:2264-2332`): agent CLI runs inside a
  Docker or Firecracker container via `sandbox_registry.exec()`.
- **Direct subprocess path** (`executor.rs:2234-2245`): override path used for
  testing — calls `provider.run()` without a sandbox.
- **Settings UI** (`handlers/ai_providers.rs:200-261, 283-363`):
  `GET /settings/ai-providers` returns provider status from `AiCliStatus`;
  `POST /settings/ai-providers/{id}/credential` encrypts and stores credentials
  inside `settings.data.agent_sandbox.providers[id]`.

This infrastructure is complete and correct for autofixer workloads. The gap
is that the general `AiService` trait (`crates/temps-ai/src/service.rs:74`) —
the seam every non-autofixer AI caller uses (`complete()`, `chat_stream()`,
`chat_stream_turn()`) — has only one implementation, `GatewayAiService`, which
always requires a direct provider API key.

### The central design question

Issue #584 asks: should Temps allow general AI workloads to route through
subscription-backed agent CLIs?

Two options:

**(a) Bridge the existing `AiCliProvider` adapter layer** to the `AiService`
trait so general workloads can route through it.

**(b) Build a new, independent provider abstraction** for the same purpose.

**Option (b) is rejected.** Duplicating provider discovery, credential storage,
auth flavor handling, `AiCliStatus` structs, installation checking, and settings
UI for a second abstraction over the same CLIs would produce two diverging
systems. The existing `AiCliProvider` trait, provider catalog, and
`agent_sandbox` credential storage are the correct abstractions — they only
need a bridge to `AiService`.

**Option (a) is adopted.** A new `AgentCliAiService` struct implements
`temps_ai::AiService` by delegating eligible operations to an
`Arc<dyn AiCliProvider>`. A `DispatchingAiService` wraps both implementations
and routes per-request based on per-scope configuration stored in
`ai_gateway_config`.

### Workload eligibility

Not all `AiService` methods can be served by an agent CLI:

| Workload | Method | Agent CLI eligible? | Reason |
|---|---|---|---|
| Alert summaries | `complete()` | Yes | Single-pass text, no tools |
| Build/deploy diagnostics | `complete()` | Yes | Single-pass text or JSON |
| Error-group titling | `complete()` | Yes | Single-pass structured text |
| `complete_typed<T>()` | `complete()` | Partial | No `response_format` enforcement; best-effort JSON extraction only |
| Tool-less streaming chat | `chat_stream()` | Yes, with caveats | `OnEventCallback` lines mapped to `TokenStream`; no tool protocol |
| Debug chat with API tools | `chat_stream_turn()` | No | Requires Temps-specific function schemas (ADR-024); agent CLIs cannot be fed an external function-calling protocol |
| Propose-then-confirm writes | `chat()` / `chat_stream_turn()` | No | `temps_write` tool requires function calling; same constraint as debug chat |

## Decision

### 1. `AgentCliAiService` bridges `AiCliProvider` to `AiService`

A new crate `temps-ai-agent-cli` introduces `AgentCliAiService`. It implements
`temps_ai::AiService` by invoking an `Arc<dyn AiCliProvider>` (the existing
trait from `temps-agents`). No new adapter trait is introduced.

```rust
// crates/temps-ai-agent-cli/src/service.rs
pub struct AgentCliAiService {
    provider: Arc<dyn AiCliProvider>,
    scratch_dir: PathBuf,     // temp dir for short-lived CLI invocations
    timeout: Duration,
    max_turns: i32,
    concurrency: Arc<Semaphore>,
}

#[async_trait]
impl AiService for AgentCliAiService {
    async fn is_available(&self) -> bool {
        let status = self.provider.get_status().await;
        status.installed && status.authenticated
    }

    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        let _permit = self.concurrency.try_acquire()
            .map_err(|_| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: "agent CLI concurrency limit reached".into(),
            })?;
        let run_dir = tempfile::tempdir_in(&self.scratch_dir)
            .map_err(|e| AiError::Provider { purpose: request.purpose.clone(),
                                              reason: e.to_string() })?;
        let cfg = AiRunConfig {
            work_dir: run_dir.path().to_owned(),
            prompt: build_prompt_for_completion(&request),
            api_key: String::new(),  // subscription mode; CLI reads ambient cred
            max_turns: self.max_turns,
            timeout: self.timeout,
            model: request.model.clone(),
            on_event: None,
        };
        let result = tokio::time::timeout(self.timeout, self.provider.run(cfg))
            .await
            .map_err(|_| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: format!("CLI timed out after {}s", self.timeout.as_secs()),
            })?
            .map_err(|e| AiError::Provider {
                purpose: request.purpose.clone(),
                reason: e.to_string(),
            })?;
        Ok(AiResponse {
            text: extract_text_from_cli_output(&result.output),
            json: try_extract_json(&result.output),
            model: result.model.unwrap_or_default(),
        })
    }

    async fn chat_stream(&self, request: ChatTurnRequest) -> Result<TokenStream, AiError> {
        if !request.tools.is_empty() {
            return Err(AiError::NotAvailable);  // tool-calling not delegatable
        }
        // spawn provider.run() with on_event → mpsc::channel → StreamExt::unfold
        // ...
    }

    // chat() and chat_stream_turn() always return Err(AiError::NotAvailable);
    // tool-calling workloads must route to the gateway.
}
```

`api_key` in `AiRunConfig` is deliberately empty. The operator's credential is
already seeded into the CLI's standard config path on the host
(`~/.claude/.credentials.json`, etc.) by the existing settings flow in
`handlers/ai_providers.rs`. The `AgentCliAiService` never reads or copies the
credential.

`complete()` calls run as short-lived direct subprocesses, matching the
`executor.rs:2234-2245` path. Full sandbox allocation (Docker/Firecracker via
`sandbox_registry.exec()`) is reserved for autofixer long-running tasks;
allocating a container per single-pass completion is prohibitively expensive.

### 2. `DispatchingAiService` routes per-scope configuration

`ai_gateway_config` already encodes scope (`"instance"`, `"project:{id}"`,
`"environment:{id}"`) and governance limits. Two nullable columns are added:

```sql
-- crates/temps-migrations/src/migration/m20260808_000001_ai_gateway_config_provider_type.rs
ALTER TABLE ai_gateway_config
  ADD COLUMN provider_type  VARCHAR NOT NULL DEFAULT 'gateway',
  ADD COLUMN agent_cli_provider_id  VARCHAR;
```

`provider_type`: `"gateway"` (existing BYOK path, default) | `"agent_cli"`.
`agent_cli_provider_id`: catalog entry id (`"claude_cli"`, `"codex_cli"`);
`NULL` when `provider_type = "gateway"`.

`DispatchingAiService` wraps both implementations:

```rust
// crates/temps-ai-agent-cli/src/dispatch.rs
pub struct DispatchingAiService {
    gateway: Arc<dyn AiService>,
    agent_registry: Arc<AgentCliRegistry>,  // keyed by provider catalog id
    db: Arc<DatabaseConnection>,
}

#[async_trait]
impl AiService for DispatchingAiService {
    async fn complete(&self, request: AiRequest) -> Result<AiResponse, AiError> {
        if let Some(svc) = self.preferred_agent_cli(request.project_id).await {
            if svc.is_available().await {
                match svc.complete(request.clone()).await {
                    Ok(r) => return Ok(r),
                    Err(e) => {
                        tracing::warn!(
                            purpose = %request.purpose,
                            reason = %e,
                            "agent CLI complete() failed; falling back to gateway"
                        );
                    }
                }
            }
        }
        self.gateway.complete(request).await
    }

    // chat_stream_turn() always delegates to gateway — tool-calling is not
    // agent-CLI-compatible (see workload eligibility table above).
    async fn chat_stream_turn(
        &self, request: ChatTurnRequest,
    ) -> Result<ChatTurnStream, AiError> {
        self.gateway.chat_stream_turn(request).await
    }
    // ...
}
```

The existing `GatewayAiService` registration at `plugin.rs:54-58` is replaced
by `DispatchingAiService`. All existing callers (`Arc<dyn AiService>`) are
unaffected — they call the same trait methods and are unaware of the dispatch.

### 3. Answers to all open design questions

**Q1: Which AI features can map to agent runtimes, and which still require
direct API access?**

Mappable (single-pass, no function-calling): alert summaries (`temps-otel`
calls `complete_text` with `purpose: "alert.summary"`), build and deploy
diagnostics, error-group titling, `complete_typed<T>()` best-effort (no
`response_format` enforcement, JSON extracted from prose output).

Not mappable: debug chat (`temps-ai-chat`, `chat_stream_turn()` with tools) and
write actions. Both require Temps-specific function-calling tools injected via
OpenAI function schema (ADR-024: `GET /api/meta/tools`, `temps_write` tool).
There is no mechanism to inject an external function schema into Claude Code's
or Codex's execution environment, and their native tool use (`bash`,
`computer_use`, `edit_file`) is not compatible with the `ToolCall` protocol
Temps' streaming handler consumes.

**Q2: Should agents run on the Temps host, inside per-job sandboxes, or through
a user-managed runner?**

For `complete()` delegation (single-pass completions): direct subprocess on the
Temps host, using the existing `AiCliProvider::run()` path. The `runuser`
privilege drop already in `claude.rs` provides adequate isolation. A throwaway
`tempdir` as the working directory prevents access to project files.

Full sandbox execution (Docker/Firecracker via `sandbox_registry.exec()`) is
unchanged and remains the path for autofixer long-running agent tasks. Allocating
a container per `complete()` call adds 1-5s of container start latency and
memory pressure that is not justified for short-lived completions on a cpx22.

User-managed runners are out of scope for this ADR.

**Q3: How should interactive authentication work for a self-hosted or remote
Temps server?**

Operators authenticate the CLI on the Temps host machine and paste the resulting
credential into `POST /settings/ai-providers/{provider_id}/credential` (the
existing UI at `/agent-sandbox/providers`). For Claude Code subscription OAuth,
`claude setup-token` generates a paste-able token. No interactive auth runs
inside the Temps server process. Device-code-based auth for operators without
SSH access is deferred to Phase 4.

**Q4: What normalized request/result protocol should adapters implement?**

The existing `AiCliProvider` trait is the adapter protocol. No new interface is
defined. `AgentCliAiService` handles the mapping: `AiRequest` fields are
composed into `AiRunConfig.prompt`; `AiRunResult.output` becomes
`AiResponse.text` with best-effort JSON extraction via `try_extract_json()`.
New providers extend the `PROVIDER_CATALOG` and implement `AiCliProvider` — the
`AiService` layer requires no changes.

**Q5: How should quotas, concurrency, cancellation, and subscription
limitations be surfaced?**

- **Concurrency**: a `tokio::sync::Semaphore` in `AgentCliAiService`, default
  capacity of 2, configurable via `ai_gateway_config.max_requests_per_minute`
  (interpreted as a concurrency cap for CLI providers; surfaced with an
  explanation in the settings UI).
- **Cancellation**: `tokio::time::timeout` wraps every `provider.run()` call
  (mirroring `claude.rs:221-228`), surfaced as
  `AiError::Provider { reason: "CLI timed out after Xs" }`. Timeout default is
  30s for `complete()`, configurable.
- **Subscription rate limits and errors**: `summarize_cli_failure()` (already at
  `ai_cli/mod.rs:111`) extracts actionable messages from CLI stderr/stdout.
  `AgentCliAiService` wraps the result in `AiError::Provider` so callers receive
  readable context. Rate-limit errors trigger fallback to the gateway in
  `DispatchingAiService`; auth errors do not (operator intervention required).
- **Token usage**: `AiRunResult.tokens_input/output` are attributed to the
  `purpose` tag in structured logs and a usage row so the admin can see CLI
  token consumption alongside BYOK usage.

### 4. Credential handling and non-extraction guarantee

`AgentCliAiService` never reads the credential. The credential stored encrypted
in `settings.data.agent_sandbox.providers[id].credentials_encrypted` is seeded
into the CLI's standard config path on the host by the existing
`session_manager::seed_provider_credentials()` flow. The CLI reads the file
directly. The only value placed in `AiRunConfig.api_key` is an empty string —
the same subscription-mode path already in use at `executor.rs:2195-2197`:
when both `api_key_encrypted` and `ai_provider_key_id` are absent, the key
field is `""` and the CLI falls back to its ambient credential.

No credential value is ever present in process arguments (`argv`), the
structured log line, or an API response. Error output is passed through
`scrub_and_bound()` (which masks `sk-ant-`, `Bearer `, etc., already in
`ai_cli/mod.rs:171-200`) before it reaches `AiError::Provider`.

### 5. Isolation, timeouts, and concurrency for `complete()` invocations

**Permission bypass flags used.** Claude Code is invoked with
`--dangerously-skip-permissions`; Codex is invoked with `--full-auto`. These are
blanket bypasses — neither CLI offers a machine-readable stdio permission
protocol suitable for unattended server use. Anthropic's Agent Client Protocol
(ACP) request for stdio-based permission callbacks was closed NOT_PLANNED
(issue #6686, February 2026). The `--permission-prompt-tool` MCP delegation flag
exists in Claude Code but has no working documented example as of this writing
(issue #1175, open). These are the only available options for non-interactive
Claude Code execution; there is no intermediate path.

**Why blanket bypass is adequate for the workloads this ADR routes.** Every
`AiService` call delegated to `AgentCliAiService` is a single-pass text
completion: alert summaries, build diagnostics, error-group titling, tool-less
streaming chat. None of these workloads invoke the CLI's native agent tools
(`bash`, `computer_use`, `edit_file`). A per-tool ACL has nothing to gatekeep
— there are no tool invocations in the prompt paths this ADR sends to the CLI.
The operative security boundary is therefore the process and workspace, not a
tool-level allowlist. That boundary is enforced by:

- `runuser` privilege drop (inherited from `claude.rs`) — no root execution
- A throwaway `tempfile::tempdir()` as working directory — no access to project
  files or host credential paths outside the CLI's own standard config location
- `tokio::time::timeout` hard deadline (default 30s)
- `on_event` is `None` — no IPC overhead for single-pass completions
- `AiRunConfig.api_key` is always `""` — subscription mode; CLI reads ambient
  credential from its own standard config path (`~/.claude/.credentials.json`
  etc.); `AgentCliAiService` never reads or copies the credential
- `Semaphore` limiting concurrent CLI processes on the host
- A 32 KB cap on the flattened prompt (`check_prompt_size`), rejected before
  any permit/tempdir/subprocess resource is acquired for the request — bounds
  the memory and permit-hold-time cost of an adversarial caller-controlled
  prompt within the semaphore's small default concurrency budget
- Error text reaching `AiError::Provider.reason` is re-scrubbed through
  `scrub_and_bound` (the same credential-pattern redaction `temps-agents` uses
  for CLI stderr) as defense-in-depth against a future error path that skips
  the upstream scrub; tempdir-creation failures return a fixed generic message
  with the real path/error logged server-side only, not returned to the caller

**Scope constraint.** This isolation posture is valid only while `AiService`
delegation to agent CLIs is restricted to single-pass, tool-less workloads. The
`chat()` method returns `Err(AiError::NotAvailable)` unconditionally — it is not
overridden, so it inherits the trait default. `chat_stream()` rejects any
request where `!request.tools.is_empty()`. `chat_stream_turn()` is explicitly
overridden to return `Err(AiError::NotAvailable)` unconditionally, rather than
inheriting the trait default that delegates to `chat_stream()`
(`crates/temps-ai/src/service.rs:106`) — relying on that default would still be
*correct* for tool-bearing requests, but would also *execute* the CLI for a
tool-less multi-turn request, since `chat_stream()`'s gate only checks
`tools.is_empty()`. The explicit override makes "multi-turn conversation entry
points never reach the CLI" a property of `AgentCliAiService` itself, not a
side effect of `chat_stream()`'s gating happening to stay correct. These guards are load-bearing: removing them without first implementing a
per-tool permission bridge (see Alternatives Considered) would expose native CLI
tools — including `bash` — to unreviewed execution on the Temps host under a
blanket-bypass flag.

For `chat_stream()` delegation, `on_event` is wired to an `mpsc::channel` that
drives the returned `TokenStream`. The same isolation and timeout apply.

### 6. Write-action safety applies uniformly

Write actions and debug chat remain on the `GatewayAiService` path at all times
— `DispatchingAiService.chat_stream_turn()` always delegates to the gateway.
`PendingActionService::confirm()` (`crates/temps-ai-chat/src/pending_actions.rs:224`)
continues to execute with the confirming user's `AuthContext` regardless of
provider. The provider is not consulted during the confirmation phase and has no
access to the confirming user's credentials or identity.

### 7. Capability endpoint and onboarding diagnostics

A new `GET /api/ai/provider-status` endpoint returns:

```rust
pub struct AiProviderStatusResponse {
    pub active_provider_type: String,         // "gateway" | "agent_cli"
    pub agent_cli_provider_id: Option<String>,
    pub configured: bool,
    pub reason: Option<String>,               // why unavailable when configured = false
    pub setup_path: Option<String>,           // "/agent-sandbox/providers" (agent CLI) or "/ai-gateway" (gateway)
    pub gateway_available: bool,
    pub agent_cli_status: Option<AiCliStatus>, // from AiCliProvider::get_status()
}
```

`AiCliStatus` (already at `ai_cli/mod.rs:53-64`) includes `installed`,
`authenticated`, `version`, `email`, `subscription_type`, and `setup_hint`.
This drives the onboarding state in the UI consistently with the existing
autofixer provider settings page.

## Consequences

### Positive

- Operators with an existing Claude Code or Codex subscription can run alert
  summaries, build diagnostics, and structured-output workloads without any
  additional API key.
- The existing `AiCliProvider` adapter, provider catalog, credential storage,
  and status reporting are fully reused — no duplicated provider management
  infrastructure.
- Adding a new agent CLI provider (e.g., a future Gemini CLI) requires only a
  `ProviderCatalogEntry` and an `AiCliProvider` impl — no changes to the
  `AiService` layer, DI, settings schema, or UI.
- BYOK gateway is preserved as the default and as the mandatory path for all
  tool-calling workloads. No existing caller changes.
- Fallback to gateway is automatic and transparent; callers cannot distinguish
  which provider served a `complete()` call.

### Negative / risks

- **Incomplete `complete_typed<T>()` fidelity.** Agent CLIs do not honour
  `response_format` enforcement. Structured output relies on prompting and
  best-effort JSON extraction. Callers that require strict schema validation
  should explicitly pin to the gateway via `ai_gateway_config`.
- **Host credential dependency.** `AgentCliAiService` depends on the CLI
  credential being valid on the Temps host. If the credential expires or is
  revoked, calls fail and fall back to the gateway (if configured), but there
  is no active rotation mechanism. The background validity poll in Phase 4
  mitigates silent degradation but does not prevent it.
- **Latency variance.** CLI process startup adds roughly 300–500ms per
  `complete()` call (fork + CLI initialisation). This is acceptable for
  alert summaries and diagnostics on cold paths; any attempt to use CLI
  delegation on a hot path would be a regression. The workload eligibility table
  must be treated as a constraint, not a guideline.
- **No subscription cost accounting.** `ai_gateway_config.max_cost_per_month_microcents`
  has no meaning for subscription providers. Token counts are recorded, but
  monetary governance is absent for this provider type. The governance story for
  subscriptions is weaker than for BYOK.
- **Concurrency is per-instance, not per-scope.** The `Semaphore` on
  `AgentCliAiService` limits concurrent CLI processes on the host. Unlike the
  gateway's per-project rate limiting, this is a coarser bound. Per-scope
  concurrency is a follow-up.
- **Blanket permission bypass accepted for tool-less workloads; not extensible
  to tool-invoking workloads without a redesign.** Claude Code is invoked with
  `--dangerously-skip-permissions` and Codex with `--full-auto`. This is
  intentional and accepted because the workloads routed through
  `AgentCliAiService` are single-pass text completions that never invoke native
  CLI tools; the bypass flag has nothing actionable to bypass. Per-tool
  granularity (as implemented by paseo's `canUseTool` SDK callback for Claude
  Code and `codex app-server` JSON-RPC approval routing for Codex) is the
  correct architecture when tool invocations are present, but the SDK dependency
  and protocol complexity are not justified for workloads where no tool calls
  occur. The guards at the `AgentCliAiService` boundary (`chat()` always
  returns `NotAvailable` via the unoverridden trait default; `chat_stream()`
  rejects non-empty tool lists; `chat_stream_turn()` is explicitly overridden
  to always return `NotAvailable`, independent of `chat_stream()`'s gating)
  are what keep the blanket bypass safe. **If those guards are relaxed in a
  future phase, the permission architecture must change before that expansion
  ships** — `--dangerously-skip-permissions` is not a valid posture for
  workloads that invoke `bash` or file-editing tools on the Temps host.

### Neutral

- Debug chat and write actions are unaffected; they continue to require a BYOK
  gateway key.
- The `agent_sandbox` settings structure that already stores provider credentials
  for the autofixer is reused without schema changes in the credential storage
  layer.

## Phased plan

### Phase 1 — Foundation: `AgentCliAiService` and no-op `DispatchingAiService`

1. Create `crates/temps-ai-agent-cli/` with `AgentCliAiService` implementing
   `AiService::is_available()`, `AiService::complete()`, and
   `AiService::chat_stream()` (tool-less only). `chat()` and
   `chat_stream_turn()` return `Err(AiError::NotAvailable)`.
2. `AgentCliAiService` takes `Arc<dyn AiCliProvider>`, a scratch `PathBuf`, a
   `Duration` timeout (default 30s), and a `Semaphore` capacity (default 2).
3. Implement `DispatchingAiService` in the same crate. In Phase 1 it wraps
   `GatewayAiService` only (no agent CLI routing). Replace the direct
   `GatewayAiService` registration at `plugin.rs:54-58` with
   `DispatchingAiService`.
4. Unit tests: mock `AiCliProvider` returning a fixed `AiRunResult`; assert
   `complete()` maps to `AiResponse` correctly; assert timeout surfaces as
   `AiError::Provider`; assert `chat_stream_turn()` returns `NotAvailable`.

### Phase 2 — Configuration: `ai_gateway_config` extension and settings UI

1. Migration: add `provider_type VARCHAR NOT NULL DEFAULT 'gateway'` and
   `agent_cli_provider_id VARCHAR` to `ai_gateway_config`.
2. Update `crates/temps-entities/src/ai_gateway_config.rs`, the config service,
   and the config handlers to read and write the new columns.
3. `GET /api/ai/provider-status` capability endpoint (authenticated,
   `SettingsRead` permission). Returns `AiProviderStatusResponse` as defined in
   Decision §7.
4. Settings UI: add an "AI Provider Preference" section that shows the current
   `provider_type`, lets the operator pick an agent CLI provider, displays
   `AiCliStatus` from `AiCliProvider::get_status()` (installed, authenticated,
   setup_hint), and links to `/agent-sandbox/providers` for credential management.
5. `DispatchingAiService.resolve_provider()` queries `ai_gateway_config` for the
   project scope (falling back to instance scope), parses `provider_type` and
   `agent_cli_provider_id`.

### Phase 3 — Live routing and fallback

1. `DispatchingAiService` activates real routing: when `provider_type =
   "agent_cli"` and `is_available()` returns true, `complete()` and
   `chat_stream()` (tool-less) delegate to `AgentCliAiService`; on
   `AiError::Provider` caused by a transient error or rate limit, fall back to
   gateway and emit a structured `tracing::warn!` with provider id and reason.
   Auth failures do not fall back automatically — they surface as errors.
2. Wire the alert-summary workload in `temps-otel`: confirm that
   `complete_text(ai, ...)` with `purpose: "alert.summary"` routes correctly
   when `provider_type = "agent_cli"`.
3. Token usage attribution: append `tokens_input/output` from `AiRunResult` to
   a usage row keyed by `purpose` and `provider_type`, visible in the admin
   usage view alongside BYOK usage.
4. Integration test: configure a mock `AiCliProvider` that returns a fixed
   result; assert an alert summary round-trips through `DispatchingAiService`
   to `AgentCliAiService` and back.

### Phase 4 — Audit, hardening, and CLI parity

1. `AiAgentExecutionAudit` struct (mirroring `AiActionConfirmedAudit` at
   `crates/temps-ai-chat/src/audit.rs:76-85`): operation type
   `"ai.agent_cli_execution"`, fields: `provider_id`, `purpose`, `model`,
   `tokens_input`, `tokens_output`, `duration_ms`, `fallback_reason` (null when
   no fallback occurred).
2. Background credential validity poll: call `AiCliProvider::get_status()` for
   the configured provider every five minutes; cache the result; surface
   staleness in the capability endpoint and the UI's onboarding state.
3. CLI error classification: distinguish rate-limit errors, auth errors, and
   transient failures in `AgentError::AiCliFailed` variants so
   `DispatchingAiService` can choose fallback (transient, rate limit) vs.
   operator-intervention required (auth failure).
4. `temps-cli` parity: `bunx @temps-sdk/cli ai provider status` showing
   configured provider, availability, fallback state, and last error.
5. Evaluate device-code-based auth for operators without SSH access to the Temps
   host; include only if the threat model is acceptable (device code exchange
   happens entirely between the operator's browser and the AI vendor — the Temps
   server only stores the resulting credential token).

## Alternatives considered

### New `SubscriptionProviderService` trait (Option B)

A new trait independent of `AiCliProvider`, defined in `temps-ai` or a new
crate. Rejected: it would duplicate the provider catalog, `AuthFlavor` /
`CredentialFormat` enums, `AiCliStatus`, installation/authentication checking,
credential encryption and storage, and the settings UI — producing two
diverging systems for managing the same CLIs. The bridge approach in this ADR
avoids all of that duplication.

### Run `complete()` calls inside the autofixer sandbox

Route `AgentCliAiService.complete()` through `sandbox_registry.exec()` (the
path at `executor.rs:2264-2332`) rather than a direct subprocess. Rejected:
container start latency is 1–5 seconds and each container holds memory for its
lifetime. Single-pass completions complete in under a second of model time;
adding a container lifecycle around every call is not proportionate. The
`runuser` + `tempdir` isolation is sufficient for a tool-less,
project-file-free completion.

### Expose `AiCliProvider` via a downcasting accessor on `AiService`

Add `fn cli_provider(&self) -> Option<&dyn AiCliProvider>` as an escape hatch
so callers that want CLI-specific behaviour can reach it. Rejected: it breaks
the object-safety and provider-agnosticism that `AiService` is designed to
provide. Callers should not need to know which provider served a request.

### Delegate all workloads — including tool-calling — to agent CLIs

Route `chat_stream_turn()` with tools to the CLI by embedding tool specs in the
prompt. Rejected: Temps' debug chat tools (ADR-024) are server-side API callers
that emit structured `ToolCall` events consumed by the Axum streaming handler.
There is no mechanism to inject an external function schema into Claude Code's
or Codex's execution environment, and their native tool use (`bash`,
`computer_use`, `edit_file`) does not conform to the `ChatStreamDelta::ToolCall`
protocol that `temps-ai-chat` consumes. A shim would be of unbounded complexity
and undefined security properties.

### Per-tool permission bridge via paseo pattern (considered; deferred)

paseo (getpaseo/paseo), the closest comparable multi-provider agent
orchestrator, implements per-tool permission control at the CLI protocol level:

- **Claude Code**: paseo drives the official `@anthropic-ai/claude-agent-sdk`
  `query()` with a `canUseTool` callback — a server-side function invoked by
  the SDK over its private stdio stream to the spawned `claude` process each
  time the agent requests a native tool. `allowDangerouslySkipPermissions` is
  set to pre-authorize a later mode switch without relaunch but does not itself
  bypass permission prompts; the gate is `canUseTool`. Five modes are supported
  (`plan`, `default`, `acceptEdits`, `auto`, `bypassPermissions`);
  `bypassPermissions` is opt-in, not the default.
- **Codex**: paseo spawns `codex app-server` (Codex's native JSON-RPC-over-
  stdio server mode) and handles `item/commandExecution/requestApproval`,
  `item/fileChange/requestApproval`, `item/tool/requestUserInput`, and MCP
  elicitation RPCs, resolving each with `{ decision: "accept" | "cancel" |
  "decline" }`. The default mode is `auto` (`approvalPolicy: "on-request"`);
  `full-access` (`approvalPolicy: "never"`) is opt-in and the docs note
  explicitly that `approval_policy: never` does not mean full filesystem
  access — the access limit comes from `sandbox_mode`.

This gives per-tool, per-operation granularity and is the correct architecture
for any agent CLI delegation that includes native tool invocations.

**Not adopted in this ADR because:**
- The workloads routed to `AgentCliAiService` are single-pass text completions.
  Native CLI tools are never invoked. There are no tool-level decisions to make,
  so per-tool gating adds no security value for the current scope.
- The SDK path for Claude Code introduces a Node.js runtime dependency into the
  Rust service host.
- `codex app-server` mode requires migrating `codex.rs` from the current
  fire-and-forget subprocess model to a stateful JSON-RPC protocol handler —
  a significant rewrite not justified for tool-less workloads.
- Claude Code's `--permission-prompt-tool` MCP flag is the documented native
  alternative, but issue #1175 documents the absence of any working reference
  implementation. Adopting it without a reference implementation is not
  acceptable for a server-side security control.

**If a future phase expands agent CLI delegation to tool-invoking workloads,**
this alternative must be implemented before that expansion ships. The specific
requirement: replace `--dangerously-skip-permissions` with either the
`canUseTool` SDK callback (for Claude Code) or a server-side MCP implementation
of `--permission-prompt-tool`, and replace `--full-auto` on Codex with
`codex app-server` mode and explicit approval routing keyed to the workload's
authorization context.

## References

- `crates/temps-agents/src/ai_cli/mod.rs:67-75` — `AiCliProvider` trait
  (adapter interface this ADR builds on; no changes needed to the trait itself)
- `crates/temps-agents/src/ai_cli/catalog.rs:1-9, 114` — `PROVIDER_CATALOG`
  extensibility invariant and catalog definition
- `crates/temps-agents/src/ai_cli/claude.rs:221-232` — `tokio::time::timeout`
  + `child.kill()` (the timeout/cancellation pattern `AgentCliAiService` adopts)
- `crates/temps-agents/src/ai_cli/mod.rs:53-64` — `AiCliStatus` struct
  (installed, authenticated, version, setup_hint — reused by capability endpoint)
- `crates/temps-agents/src/ai_cli/mod.rs:111` — `summarize_cli_failure()`
  (error classification for rate limits, auth failures, transient errors)
- `crates/temps-agents/src/services/executor.rs:2161-2197` — three-tier key
  resolution (model for subscription-mode empty-key path)
- `crates/temps-agents/src/services/executor.rs:2234-2245` — direct
  `provider.run()` path (subprocess execution model `AgentCliAiService` adopts)
- `crates/temps-agents/src/handlers/ai_providers.rs:200-261` — existing provider
  status + catalog endpoint (reused by Phase 2 settings UI)
- `crates/temps-ai/src/service.rs:74` — `AiService` trait (`AgentCliAiService`
  implements this)
- `crates/temps-ai-gateway/src/plugin.rs:54-58` — DI registration replaced by
  `DispatchingAiService` in Phase 1
- `crates/temps-ai-chat/src/pending_actions.rs:224` — `confirm()` applies
  uniformly; write actions remain on the gateway path regardless of this ADR
- `crates/temps-ai-chat/src/audit.rs:76-85` — `AiActionConfirmedAudit` (shape
  mirrored by Phase 4 `AiAgentExecutionAudit`)
- `crates/temps-entities/src/ai_gateway_config.rs` — entity extended in Phase 2
  with `provider_type` and `agent_cli_provider_id` columns
- ADR-022 — `AiService` foundation and `GatewayAiService` (what
  `DispatchingAiService` wraps on the gateway path)
- ADR-024 — API-as-LLM-tools (explains why debug chat cannot delegate to agent
  CLIs; function schema injection is not possible)
- ADR-036 — persistent workspace sandboxes (sandbox lifecycle unchanged; only
  the autofixer long-running path uses it)
- Claude Code CLI issue #6686 — ACP (Agent Client Protocol) stdio-based
  permission callbacks, closed NOT_PLANNED by Anthropic, February 2026; explains
  why no machine-readable mid-turn permission protocol exists for unattended
  Claude Code invocations (`--dangerously-skip-permissions` is not a design
  choice but the only available non-interactive option)
- Claude Code CLI issue #1175 — `--permission-prompt-tool` MCP delegation flag:
  feature exists but has no documented working example as of this writing; the
  underdocumented alternative evaluated and deferred pending a reference
  implementation
- Claude Code CLI issue #19978 — contradictory security guidance on
  `--dangerously-skip-permissions` (airgapped vs. egress-filtered contexts); no
  vendor guidance for multi-tenant or PaaS deployment scenarios exists
- paseo `getpaseo/paseo` — `packages/server/src/server/agent/providers/claude/
  {agent.ts,query.ts}` and `providers/codex-app-server-agent.ts` — reference
  implementation for the per-tool permission bridge pattern (paseo's `canUseTool`
  SDK callback for Claude Code and `codex app-server` JSON-RPC approval routing
  for Codex); evaluated and deferred to a future phase if tool-invoking
  delegation is ever added
