---
title: "ADR-023: Persistent AI debugging conversations (chat per interaction)"
status: Proposed
date: 2026-06-27
author: David Viejo
---

# ADR-023: Persistent AI debugging conversations (chat per interaction)

**Status:** Proposed
**Date:** 2026-06-27
**Author:** David Viejo

## Context

We want AI-assisted debugging in Temps: **a resumable chat per interaction** — one
per deployment failure first, then generally one per any entity the user is
looking at (alert, error group, …) — that the user can reopen and continue.

This is a step beyond the one-shot `diagnose_failure` helper (ADR-022): it needs
persisted multi-turn history, streaming replies, context seeding from the entity,
and a clean OSS/EE line. A parallel code survey (6 read-only explorers) established
what exists:

- **No AI SRE companion is built yet** — only the ADR-022 foundation and a defined
  but unemitted `TelemetryEventKind::AiSreConversationStarted`
  (`crates/temps-core/src/telemetry.rs:78`). We are building the chat, not
  generalizing an existing one.
- **A dormant conversations+messages schema already exists**: `workspace_sessions`
  + `workspace_messages` (feature removed) at
  `crates/temps-migrations/.../m20260421_000001_squash_apr_post_v006.rs:68-156` —
  `messages(session_id FK, role VARCHAR(20), content TEXT, metadata JSONB, created_at)`
  with a `(session_id, created_at)` composite index. A ready template.
- **Streaming is production-ready** in the gateway:
  `GatewayService::chat_completion_stream` (`gateway_service.rs:146`) →
  `Stream<Result<Bytes, AiGatewayError>>`; the HTTP handler streams SSE via
  `Body::from_stream` with usage tracking on stream-end (`handlers/gateway.rs:107-195,409-473`).
  Multi-turn is already native (`ChatCompletionRequest.messages: Vec<ChatMessage>`,
  roles system/user/assistant/tool). But **`temps-ai`'s `AiService` is
  request/response only** (`crates/temps-ai/src/service.rs:69-78`) — it needs a
  streaming, multi-turn method.
- **The deploy-failure path is OSS and chat-friendly**: `deployments.state`
  (`failed`) + `cancelled_reason`; per-step `deployment_jobs.{status,log_id,error_message}`;
  logs via `LogService.get_log_content(log_id)`. Failed deployments are *never*
  torn down (`mark_deployment_complete.rs:1531`), so logs survive for long-lived
  chats. Routes follow `/projects/{pid}/deployments/{id}/*` + `permission_guard!`.
- **A chat UI pattern exists**: `web/src/components/agents/AutopilotRunDetail.tsx`
  (streaming event parse, feedback textarea, tokens/cost, `MessageSquare`), plus a
  generated SSE client `web/src/api/client/core/serverSentEvents.gen.ts`.
  `web/src/pages/DeploymentDetails.tsx` is where "Debug with AI" mounts.
- **The Autofixer is OSS** (`crates/temps-agents`: `agent_runs`/`agent_run_logs`,
  Claude-CLI `--continue` resume, SSE-via-`get_logs_after` polling) and is the
  natural hand-off ("open a fix PR").

### Rejected alternatives

- **Reuse `agent_runs`/`agent_run_logs` for chat storage.** Rejected: that schema
  is task-execution-shaped (`phase`, `branch_name`, `pr_url`, `analysis`); logs are
  `level`-typed, not `role`-typed turns. Forcing chat into it clutters both. We add
  a purpose-built conversations/messages schema and *link* to an autofixer run when
  a chat delegates a fix.
- **One bespoke table per context** (`deployment_debug_chats`, `alert_chats`, …).
  Rejected: the requirement is "a chat for *every* interaction." A polymorphic
  `(context_type, context_id)` generalizes without N tables.
- **Provider-side session resume** (Claude-CLI `--continue`, as the autofixer does).
  Rejected for the gateway chat: it's sandbox/CLI-specific. The BYO-key gateway is
  OpenAI-compatible and stateless, so we **replay stored history** each turn
  (send `messages[]`), which is provider-agnostic and the source of truth is our DB.

## Decision

**Add a generic, persisted, streaming conversation system — `ai_conversations` +
`ai_messages` keyed by a polymorphic `(context_type, context_id)` — in a new
`temps-ai-chat` crate built on the `temps-ai` foundation. Seed each conversation
from its entity via a `ConversationContextProvider` (deployment-failure first),
stream replies over SSE by replaying stored history through the gateway, and keep
the OSS/EE line: chat + explain is OSS (BYO key, opt-in); proactive cross-signal
investigation is EE.**

### 1. Data model (`temps-entities` + migration)

Generalize the dormant `workspace_messages` shape:

```sql
CREATE TABLE ai_conversations (
  id            BIGSERIAL PRIMARY KEY,
  public_id     TEXT NOT NULL UNIQUE,          -- url-safe id
  project_id    INT  NOT NULL,
  context_type  TEXT NOT NULL,                 -- 'deployment' | 'alert' | 'error_group' | 'general'
  context_id    TEXT NOT NULL,                 -- the entity id (string; ints stringified)
  title         TEXT,                          -- auto-summarized from first turn
  status        TEXT NOT NULL DEFAULT 'active',-- 'active' | 'archived'
  created_by    INT,                           -- user id (nullable for system-started)
  metadata      JSONB,                         -- seed refs (log_ids, deployment state, etc.)
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_ai_conversations_context ON ai_conversations(project_id, context_type, context_id);

CREATE TABLE ai_messages (
  id              BIGSERIAL PRIMARY KEY,
  conversation_id BIGINT NOT NULL REFERENCES ai_conversations(id) ON DELETE CASCADE,
  role            TEXT NOT NULL,               -- 'system' | 'user' | 'assistant' | 'tool'
  content         TEXT NOT NULL,
  metadata        JSONB,                       -- structured diagnosis, tool calls, citations
  tokens_in       INT,
  tokens_out      INT,
  cost_microcents BIGINT,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_ai_messages_conversation ON ai_messages(conversation_id, created_at);
```

`(context_type, context_id)` makes "a chat per interaction" a lookup, not a new
table per surface. Per-message `tokens_*`/`cost_microcents` give per-conversation
cost (user-controlled spend, §5).

### 2. `temps-ai` gains streaming + multi-turn

`AiService` today is single request/response. Add a multi-turn streaming method
(object-safe; the gateway already supports both):

```rust
pub struct ChatTurnRequest {
    pub purpose: String,
    pub project_id: Option<i32>,
    pub messages: Vec<ChatMessage>,   // {role, content} — full replayed history
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}
pub type TokenStream = Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>>;

#[async_trait]
pub trait AiService {
    // … existing is_available / complete …
    async fn chat_stream(&self, req: ChatTurnRequest) -> Result<TokenStream, AiError>;
}
```

`GatewayAiService::chat_stream` calls `chat_completion_stream`, parses the
OpenAI SSE chunks (`ChatCompletionChunk.choices[].delta.content`) into a token
stream — reusing the gateway's existing SSE buffering. `ChatMessage`/role types
move to (or are re-exported from) `temps-ai` so consumers don't depend on the
gateway.

### 3. `temps-ai-chat` crate — the conversation service

New crate depending on `temps-ai` + `temps-entities` (not the gateway directly;
it gets `Arc<dyn AiService>` via DI):

- `ConversationService`: create/find-by-context/get-history/append-message;
  `send_message(conversation, user_text) -> TokenStream` that (a) loads history,
  (b) prepends the context provider's fresh system context, (c) `ai.chat_stream(...)`,
  (d) persists the assistant message on stream-end (mirroring the gateway's
  usage-tracking-on-end at `handlers/gateway.rs:154-194`).
- **`ConversationContextProvider` trait** (DI-registered, keyed by `context_type`):

  ```rust
  #[async_trait]
  pub trait ConversationContextProvider: Send + Sync {
      fn context_type(&self) -> &'static str;          // "deployment"
      async fn authorize(&self, project_id: i32, context_id: &str, auth: &AuthCtx) -> bool;
      async fn seed(&self, project_id: i32, context_id: &str) -> ConversationSeed; // system prompt + first assistant msg
      async fn live_context(&self, project_id: i32, context_id: &str) -> String;   // refreshed each turn
  }
  ```

  The **deployment provider** (first) lives near `temps-deployments`: `seed` pulls
  the failed `deployment_jobs` logs via `LogService`, runs `temps_ai::diagnostics::diagnose_failure`,
  and returns `{ system: failure-context, first_assistant: rendered diagnosis }`.
  Adding alerts/error-groups later = another provider, no schema change.

### 4. HTTP API (OSS surface)

Under `/projects/{project_id}/ai/conversations` (auth + `permission_guard!`, the
provider's `authorize` enforces context-level access):

- `GET ?context_type=&context_id=` → find/list (so a page opens the existing chat).
- `POST` `{context_type, context_id}` → create (idempotent per context; lazy — created on first open).
- `GET /{id}` → full message history.
- `POST /{id}/messages` `{content}` → **SSE stream** of the assistant reply
  (`text/event-stream`, mirroring `handlers/gateway.rs` / `log_handler.rs` SSE), then
  persisted. Gated: `ai.is_available()` + the per-project opt-in (§5) → 409/empty if off.
- `POST /{id}/archive`, `DELETE /{id}` (audit-logged).

### 5. User control + OSS/EE boundary

Per ADR-022 and the pricing-honesty stance:

- **Off until configured + opted in.** Capability gate `ai.is_available()` (a
  provider key exists); feature gate a per-project toggle (`projects.ai_debug_chat_enabled`,
  tri-state like `ai_alert_summaries_enabled`); spend is governed by `ai_gateway_config`
  and surfaced per-conversation via the `tokens/cost` columns. It's the user's key,
  cost, and scope.
- **OSS** = the conversation system + the deployment-failure debugging chat
  (explain, Q&A, with the user's own key). This is the adoption hook; gating it
  behind EE would repel.
- **EE (`Feature::AiSre`)** = the *proactive, cross-signal investigator*: a chat
  that autonomously pulls correlated traces/metrics/errors, reasons across signals,
  and runs unattended — the `AiSreConversationStarted` event already reserves the
  name. Same `temps-ai-chat` substrate; the EE crate adds richer context providers
  + autonomy.
- **Hand-off to the Autofixer** ("open a fix PR") stays OSS (the autofixer is
  un-gated) — the chat can spawn an autofixer run and link it via
  `ai_conversations.metadata.autofixer_run_id`.

### 6. Frontend

- A reusable `<DebugChat contextType contextId />` mounted on
  `DeploymentDetails.tsx` (a "Debug with AI" button on failed deployments), built
  by mirroring `AutopilotRunDetail.tsx`'s message list + feedback textarea and
  consuming the SSE message endpoint via the generated `serverSentEvents.gen.ts`
  client. New endpoints are defined in the backend OpenAPI and **consumed through
  the regenerated SDK** (never hand-rolled fetch).
- The same component later mounts on alert/error-group pages by passing a
  different `contextType`.

## Consequences

### Positive
- One resumable chat per interaction, generic across entities, with persisted
  history and streaming — the requested model.
- Reuses a dormant-but-proven schema, the gateway's streaming, the autofixer's
  SSE/hand-off, and the Autopilot chat UI — little net-new plumbing.
- Clean layering: `temps-ai` (foundation + streaming) → `temps-ai-chat`
  (conversation + providers) → domain providers + UI. New surfaces = new provider.
- OSS adoption hook with a protected EE upgrade (proactive cross-signal AiSre).

### Negative / risks
- **Cost on a human-driven loop**: every turn spends the user's tokens. Mitigated
  by opt-in, `is_available`, per-project budgets, history truncation (sliding window
  + always-keep seed), and visible per-conversation cost.
- **Prompt-injection / data exposure**: logs go into prompts; providers must pass
  only intended data and redact secrets in the seed. Document the trust boundary.
- **Streaming/persistence consistency**: if the client disconnects mid-stream, the
  partial assistant message must still persist (reuse the gateway's on-end spawn).
- **History growth**: long chats need truncation for the model and pagination for
  the UI; the `(conversation_id, created_at)` index supports both.
- **New crate + 2 tables + streaming + UI** is a multi-layer feature; phase it.

### Neutral
- No behaviour change when no key is configured or the toggle is off — the chat UI
  simply doesn't offer itself.

## Phased plan

1. **P1 — deployment-failure chat (OSS, end-to-end):** migration + entities
   (`ai_conversations`/`ai_messages`); `chat_stream` in `temps-ai` + gateway;
   `temps-ai-chat` crate (service + provider trait); the deployment context
   provider (seed = `diagnose_failure` + job logs); the 4 endpoints (SSE send);
   `projects.ai_debug_chat_enabled` toggle; `<DebugChat>` on `DeploymentDetails`.
2. **P2 — generalize:** alert + error-group providers (no schema change); the chat
   component mounted on those pages; conversation list/archive UI.
3. **P3 — autonomy + EE:** "open a fix PR" hand-off to the Autofixer (OSS);
   EE `Feature::AiSre` proactive cross-signal investigator providers.

## Open questions
1. **Toggle exposure**: ship `ai_debug_chat_enabled` settable via the project API +
   a settings switch in P1, or SQL-only until P2? (Recommend P1 — it's the control surface.)
2. **History truncation policy**: fixed sliding window of last N turns + always the
   seed, or token-budget-based? Summarize older turns into the seed?
3. **Auto-start vs lazy**: create the deployment chat eagerly on failure (so a
   notification can deep-link) or lazily on first open? (Recommend lazy + a "Debug
   with AI" entry point; eager later if we deep-link from alerts.)
4. **Drop the dormant `workspace_*` tables** in the same migration, or leave them?
5. **Default model** for chat — reuse `ai_gateway_config.allowed_models[0]` (ADR-022)
   or a dedicated chat model setting?

## References
- ADR-022 (AI foundation) — `temps-ai` `AiService`, `complete_typed`, `diagnose_failure`;
  this builds the streaming/multi-turn + conversation layer on top.
- `crates/temps-migrations/.../m20260421_000001_squash_apr_post_v006.rs:68-156` —
  dormant `workspace_sessions`/`workspace_messages` schema (the template).
- `crates/temps-ai-gateway/src/services/gateway_service.rs:146` — `chat_completion_stream`;
  `src/handlers/gateway.rs:107-195,409-473` — SSE streaming + usage-on-end.
- `crates/temps-ai/src/service.rs:69-78` — `AiService` (needs `chat_stream`).
- `crates/temps-entities/src/{deployments.rs,deployment_jobs.rs}` — failure state +
  `log_id`; `LogService.get_log_content()` for seed context.
- `crates/temps-agents` — Autofixer (`agent_runs`/`agent_run_logs`, `--continue`,
  `get_logs_after` SSE) — hand-off target; OSS, un-gated.
- `web/src/components/agents/AutopilotRunDetail.tsx` (chat UI to mirror),
  `web/src/api/client/core/serverSentEvents.gen.ts` (SSE client),
  `web/src/pages/DeploymentDetails.tsx` (mount point).
- `crates/temps-core/src/telemetry.rs:78` — reserved `AiSreConversationStarted` (EE).
