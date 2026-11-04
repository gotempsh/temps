# temps-ai-chat

Chats between a person and a model. Adds to the root
[`AGENTS.md`](../../AGENTS.md); unattended runs are in
[`temps-ai-agents`](../temps-ai-agents/AGENTS.md).

## Owns

- Conversations, turns and the tool loop; approval-gated writes
  (`ai_pending_actions`), secret slots, goals, compaction, attachments.
- Workspaces (`ai_applications`), blueprints, and a chat's linked sandbox.
- Autofix incidents (`/projects/{project_id}/autofix/*`).
- `harness/`: playbooks, indexed in the system prompt and read through the
  `temps_playbook` tool (`src/harness.rs`). Elsewhere "harness" means the agent
  CLI running a turn.

Not here: agent runs, `/autofixer`, provider sign-ins and saved credentials
(`temps-ai-agents`; neither crate depends on the other); `AiService`
(`temps-ai`, implemented by `temps-ai-gateway`); CLI drivers (`temps-ai-cli`) and
their bridge (`temps-ai-agent-cli`); the API as tools (`temps-ai-api-tools`);
sandboxes (`temps-sandbox`, `temps-sandbox-providers`); MCP for outside clients
(`temps-mcp-server`).

## Entry points

- `src/plugin.rs`: `AiChatPlugin`, the provider list, workers, route assembly.
- `src/service/mod.rs`: `ConversationService` (`send_message`, `claim_turn`,
  `cancel_turn`), `refresh_tool_authorization`.
- `src/service/tool_loop/`: a turn's stages. `src/service/tool_dispatch.rs`:
  `dispatch_conversation_tool`, `public_tool_result`.
- `src/provider.rs`: `ConversationContextProvider`; `src/providers/` has one per
  context type. `src/handlers/`: `configure_routes`, `access.rs`.
- `src/pending_actions.rs`, `src/secret_slots.rs`, `src/sensitive.rs`,
  `src/sandbox_link.rs`, `src/autofix/service.rs`, `src/workspace_chats.rs`.

## Invariants

- **A chat belongs to its creator.** Lookups bind `created_by`, so a project member
  cannot read or resume it:
  `every_conversation_lookup_is_scoped_to_the_current_creator`.
- **Access is rechecked per tool call.** A captured `AuthContext` would outlive a
  revoked key: `the_tool_executor_refreshes_the_credential_before_each_call`,
  `project_revocation_stops_an_active_native_provider_stream`.
- **The model cannot authorize a write.** Auto mode never approves a DELETE or a
  `secret_ref:` spend, which could aim a key at the model's host:
  `platform_auto_approval_never_spends_a_provided_secret`.
- **Repository reads stay out of the transcript;** they use a server-side token:
  `public_tool_results_hide_privileged_read_data`.
- **One claim per turn,** or two tabs start two harnesses:
  `claim_turn_rejects_an_idempotent_duplicate_without_appending_a_message`. An
  interrupted round runs no tools: `an_interrupted_round_never_runs_its_tool_calls`.
- **The MCP bridge has no session.** A per-turn bearer dies with the turn, or a
  sandbox keeps the user's tools:
  `harness_mcp_capability_is_authenticated_scoped_and_removed_with_guard`.
- **Raw provider errors stay in logs;** they can hold paths and credentials:
  `live_wire_classifies_provider_failures_without_exposing_raw_diagnostics`.
- **The model cannot pick a sandbox,** and linked credentials are cut from output:
  `application_workspaces_agent_runs_and_destroyed_sandboxes_are_not_linkable`,
  `a_command_that_prints_its_environment_does_not_show_the_password`.
- **Nobody watches an incident chat,** so tools are refused at dispatch:
  `an_incident_chat_refuses_unattended_tools_at_dispatch`. `close_out` deletes its
  sandbox and `reconcile_closed` retries; no test pins these.
- **Playbooks match the code.** One that fails to parse is dropped at runtime:
  `every_embedded_playbook_parses`, `playbooks_only_name_tools_that_exist`.

## Do not

- Add an unattended-unsafe tool without listing it in `tool_refused_in_incident`.
- Put task guidance in Rust prompts: add a file to `harness/` and to `SOURCES`.
- Auto-answer `temps_ask_user` (`ask_user_is_never_answered_automatically`).
- Add a route without a `handler_group_routes` entry; that list is kept by hand.

## Testing

`cargo test --lib -p temps-ai-chat <name>`. `workspace_chats.rs` and
`handlers/user_route_authorization_tests.rs` also use `TestDatabase` and skip
without a container runtime; the rest use `MockDatabase`. `harness.rs` tests
`include_str!` other crates' source files. CI: `unit-b` (`kind(=lib)`); there is
no `tests/` binary or allow-list entry.
