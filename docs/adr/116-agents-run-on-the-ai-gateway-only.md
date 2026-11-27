<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-116: Agents run on the AI gateway only

**Status:** Proposed
**Date:** 2026-11-24
**Supersedes:** [ADR-037](./037-subscription-agent-cli-providers.md)
(subscription-backed agent CLI providers);
[ADR 113](./113-runtime-model-catalog-and-coding-agent-updates.md), only its
coding-agent update half (the runtime model catalog stays).

Related: ADR-022 (the `AiService` seam), ADR-023 (AI conversations), the
autofix incidents in `crates/temps-ai-chat/src/autofix`.

## Context

Temps reaches models in two ways:

1. **The AI gateway** (`temps-ai-gateway`). Provider keys, models, pricing,
   usage and limits are configured once, and every model call is metered and
   governed in one place. Chat, summaries, titles and the incident autofix
   already run on Temps' own tool loop
   (`crates/temps-ai-chat/src/service/tool_loop`) against it.
2. **Harnesses**: third-party coding CLIs (Claude Code, Codex, OpenCode) that
   Temps installs, signs in, keeps updated and drives. These add:
   - **Sign-ins.** "Sign in with Claude" runs `claude auth login --claudeai`
     on the server. Paste token runs `claude setup-token`, and a CLI login
     already on the host can be imported. Temps stores an encrypted
     access-token snapshot, and the refresh token stays in the CLI's own
     directory on the server (`temps-ai-cli/src/{login,credential_snapshot}.rs`,
     `temps-ai-agents/src/services/provider_{login,credential}_service.rs`).
   - **A second AI path.** `AiProviderRegistry` dispatches a turn either to the
     gateway or to a CLI adapter (`temps-ai-agent-cli/src/dispatch.rs`). The
     sandbox model relay (`model_relay.rs`) hands CLIs in sandboxes a scoped
     bearer, and the gateway maps its provider keys to
     `SandboxHarnessCredentials` (`temps-ai-gateway/src/plugin.rs`).
   - **Agent runs**, which execute a CLI inside a sandbox. These are project
     agents, their workflows, cron and error-group triggers, and the original
     autofixer (`temps-ai-agents/src/services/{executor,autofixer}.rs`,
     ~9.5k lines).
   - **Upkeep.** Version checks and auto-updates of the CLIs
     (`temps-ai-gateway/src/services/coding_agents`, `harness_update.rs`),
     preflight checks, and per-CLI model catalogs and settings
     (`agent_sandbox.providers["claude_cli" | "codex_cli" | "opencode"]`).

The harness path costs us the most code and support load for the least
control:

- **Code.** `temps-ai-cli` (9.5k lines) and `temps-ai-agent-cli` (14k) exist
  only for it, along with most of `temps-ai-agents` (35k) and a dozen console
  pages and components.
- **Fragility.** Each CLI changes its flags, output and login flow on its own
  schedule. We parse its stream, track its releases and debug its sign-in on
  servers we do not see.
- **Governance.** Usage on a subscription sign-in bypasses the gateway's
  pricing, limits and usage accounting. A harness run is the one model call
  Temps cannot meter or cap.
- **Onboarding.** First run has to explain sign-in, paste token, import and
  API key across three vendors before an agent can do anything.

There are also two autofixers. The incident autofix already does the whole
job on the gateway with Temps' own sandbox tools: it reproduces and fixes the
crash in a sandbox, then scores, delivers and records the result. The CLI one
duplicates it.

## Decision

### D1. One way to reach a model: the AI gateway

Every model call Temps makes goes through the gateway: chat, summaries,
titles, autofix and agents. The way to connect a model is a provider key, or
any OpenAI-compatible endpoint, configured in the gateway. There are no
subscription sign-ins, pasted CLI tokens, imported CLI logins or
harness-specific credentials.

`AiProviderRegistry` stops dispatching: `AiService` is the gateway. The
`provider` field on a chat turn names a gateway provider or model, never a
CLI.

### D2. Agents are Temps agents: the tool loop, in a sandbox

An agent is a headless conversation on Temps' own tool loop. It works inside
its sandbox through the existing sandbox tools (`sandbox_exec`,
`sandbox_read_file`, `sandbox_write_file`, and others), with its model from the
gateway. Nobody installs, signs in or updates a CLI.

- **The autofixer is the incident autofix.** The CLI autofixer
  (`temps-ai-agents/src/services/autofixer.rs`) is removed. Its entry points
  ("Autofix" on an error group, its triggers and settings) open or resume an
  autofix incident.
- **Project agents** (definitions, workflows, cron and error-group triggers)
  keep their definitions and triggers. A run keeps its clone, sandbox, push
  and pull request steps. Only the step that ran a CLI changes: it runs a
  tool loop (`temps-ai-agents/src/services/agent_loop.rs`). Each model turn
  is one `AiService::chat` call on the gateway, with the same three sandbox
  tools a chat has (`sandbox_exec`, `sandbox_read_file`,
  `sandbox_write_file`), backed by the run's own sandbox. `agent_runs` and
  `agent_run_logs` record it as today. The transcript is the run's output,
  one JSON object per line (`temps.text`, `temps.tool_call`,
  `temps.tool_result`, `temps.finish`), and the run viewer replays it. The
  loop lives with the runs rather than in `temps-ai-chat`, so the agents
  crate does not take on the chat crate as a dependency.
- **Turn caps and time budgets** are enforced by the loop: `max_turns` model
  turns and the agent's `timeout_seconds`, for every model. Before this, only
  Claude Code honoured a turn cap.

### D3. A sandbox stays a plain machine

Sandboxes, their providers, previews, terminals and the git credential bundle
stay. Temps stops putting a harness in them: no CLI install, no model relay,
no `SandboxHarnessCredentials`. Someone who wants a coding CLI in their own
sandbox terminal installs it themselves and points it at the gateway's
OpenAI-compatible endpoint with a gateway key. That is ordinary use of the
gateway, and Temps does not manage it.

### D4. What is removed

| Removed | Where |
|---|---|
| CLI providers, logins, credential snapshots, CLI catalog | the whole `temps-ai-cli` crate |
| Gateway/CLI dispatch, sandbox model relay, harness updates, runtime transport | the whole `temps-ai-agent-cli` crate |
| CLI executor, CLI autofixer, provider login and credential services, sandbox CLI injection | `temps-ai-agents/src/services/{executor (CLI parts), autofixer, provider_login_service, provider_credential_service, sandbox_injector}.rs` and their handlers |
| Coding-agent version checks and auto-update | `temps-ai-gateway/src/services/coding_agents`, `SandboxHarnessCredentials` mapping in its `plugin.rs`, CLI preferences in `provider_preference_service.rs` |
| CLI access rules in chat | `temps-ai-chat/src/handlers/access.rs`, `service/mod.rs` (`find_provider`), the model relay routes in `plugin.rs` |
| CLI settings | `agent_sandbox.providers` in `temps-config`, `ai_agent_cli_settings` |
| Console | `web/src/pages/agent-sandbox/Harness*`, `harness-*`, `provider-model-catalog`, `web/src/lib/ai-cli-providers.ts`, the harness parts of `components/ai`, `components/ai-first`, `components/autofixer` (`AutofixRunConfigForm`), and the welcome step's "Sign in to a harness here" |
| CLI commands | the `temps-cli` and `apps/temps-cli` commands for harness login, import and update |

`temps-ai-agents` keeps agent definitions, triggers, cron, run records and
the sandbox providers, re-exported from `temps-sandbox-providers`. It loses its
`temps-ai-cli` dependency and gains `temps-ai-chat`'s headless conversation
entry point (D2). If what remains is small, it merges into `temps-ai-chat` in
a later change, not this one.

### D5. Existing data and installs

- **Credentials.** A migration deletes the stored CLI credential snapshots
  and the `ai_agent_cli_settings` rows. The CLI's own credential files under
  the data directory are deleted at startup, once, and an audit entry is
  written. The refresh token in them is a live subscription credential, so
  leaving it on disk after the feature is gone is a liability.
- **Saved choices.** A project agent, autofix setting or chat default that
  names `claude_cli`, `codex_cli` or `opencode` falls back to the gateway's
  default model. The console says so once: "This agent used Claude Code. It
  now runs on <model> through the AI gateway."
- **No gateway provider.** Agents, autofix and chat render their onboarding
  state: "No AI provider is configured", linking to `/settings/ai` (AGENTS.md
  Feature Discoverability). They never show a disabled control.
- **API.** The login, import, credential, harness-update and model-relay
  routes are removed, not deprecated: they are console-only and admin-only.
  The OpenAPI delta is in the PR, and both clients are regenerated.
- **Tables.** `agent_runs` and `project_agents` stay (D2). The harness-only
  columns are dropped in the same migration.

### D6. Onboarding: connect a model, not a harness

The welcome step "Connect an agent" offers two choices: **Add an AI provider**
(a provider key, the same inline form as `/settings/ai`) and **Later**. The
`AgentWay` values `harness` and `here` are replaced by `gateway`.

## Consequences

- About 25k lines (`temps-ai-cli` and `temps-ai-agent-cli`) and most of the
  CLI half of `temps-ai-agents` are deleted, with the matching console pages,
  tests and settings.
- Every model call is metered, priced and capped by the gateway.
- **Users lose subscription-backed agents.** Anyone running agents on a
  Claude or ChatGPT subscription needs a provider API key instead. This is the
  main cost of the decision, and the release notes say it plainly.
- Agent quality now depends on Temps' tool loop and playbooks, not on a
  vendor's harness. Gaps the CLIs covered show up as missing sandbox tools
  (search across files, apply a patch) and are added there, once, for every
  model.
- The proxy, sandboxes and workers are unaffected.

## Plan

Each phase compiles, passes its tests and is shippable on its own.

1. **Agents on the tool loop.** Run project agents through the loop (D2), stop
   putting model credentials in run sandboxes (D3), and point the console's
   "Fix with AI" address and the error alarm's action at the error group,
   where incidents start. The CLI code still exists, but no agent run reaches
   it. *Done.*
2. **One AI path.** Make `AiService` the gateway (D1). Remove CLI choices from
   chat, settings and the welcome step (D6), and fall saved CLI choices back
   (D5).
3. **Delete.** Remove `temps-ai-cli`, `temps-ai-agent-cli`, the CLI services,
   handlers, console pages and CLI commands (D4). Add the credential and
   settings migration and the one-time file cleanup (D5). Regenerate both API
   clients and update `docs/architecture/crates.md`, `scripts/crate_map.py` and
   the rule baseline.

## Alternatives considered

- **Keep the CLIs but authenticate them only through the gateway.** This
  removes the sign-ins but keeps the install, update, stream-parsing and
  relay code, which is most of the cost. It also keeps two agent runtimes.
- **Keep subscription sign-in as an opt-in.** This keeps the code and the
  ungoverned model path for one way of paying, so it was rejected.
