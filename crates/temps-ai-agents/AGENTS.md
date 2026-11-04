# temps-ai-agents

Unattended AI work: agents, runs, triggers and the autofixer. Adds to the root
[`AGENTS.md`](../../AGENTS.md); chats with a person are in
[`temps-ai-chat`](../temps-ai-chat/AGENTS.md).

## Owns

- Agents, runs and triggers (`project_agents`, `agent_runs`), and CLI dry runs.
- Autofixer runs (`/projects/{project_id}/autofixer/*`): analyze, fix and open a
  pull request as phases of one `agent_runs` row.
- Agent secrets, skill and MCP definitions, provider credentials and sign-ins
  (`/settings/ai-providers/*`, also used by chat turns), the preview gateway.
- Picking the host's backend (`build_sandbox_provider`) and registering
  `dyn SandboxProvider` and `SandboxRegistry` for other plugins.

Not here: chats and autofix incidents (`temps-ai-chat`; neither crate depends on
the other); backends and `SandboxRegistry` (`temps-sandbox-providers`, re-exported
as `crate::sandbox`); the `sandboxes` table (`temps-sandbox`); CLI drivers
(`temps-ai-cli`, re-exported as `crate::ai_cli`); `AgentError`
(`temps-agent-core`); the memory contract (`temps-memory`).

## Entry points

- `src/plugin.rs`: `AgentsPlugin`, `process_jobs`, `evaluate_trigger`.
- `src/services/executor.rs`: `AgentExecutor` (`execute_run`,
  `prepare_sandbox_workspace`). `src/services/run_service.rs`: `AgentRunService`.
- `src/services/`: `AutofixerService`, `AgentConfigService`, `DefinitionService`,
  `SecretService`, `ProviderLoginService`.
- `src/handlers/mod.rs`: `AgentsApiDoc`, `configure_public_routes` (webhook).
- `src/preview_gateway.rs`: `spawn_reconcile`.

## Invariants

- **A sandbox never holds a broad Git credential.** A PAT reaches every repository
  of its owner, so only a read-only, one-repository token is given:
  `pat_and_oauth_connections_give_the_sandbox_no_token`.
- **A run token asks for nothing,** so a sandbox cannot call the Temps API:
  `run_token_permissions_are_minimal`.
- **Docker-socket-granted projects are refused** at `prepare_sandbox_workspace`.
  The test pins the function, not that call:
  `refuse_granted_project_push_refuses_a_project_this_host_declares`.
- **Runs see only their project's and instance-wide secrets:**
  `runs_only_receive_their_own_project_and_instance_wide_secrets`.
- **Run output is redacted by value,** since agents can print their environment and
  members read the logs: `append_log_redacts_registered_secret_values_by_value`. A
  phase whose secrets cannot be resolved does not start:
  `fix_phase_fails_closed_when_secrets_cannot_be_resolved`.
- **Another project's records read as missing:**
  `test_ensure_run_in_project_mismatch_returns_run_not_found`,
  `a_foreign_error_group_reads_exactly_like_a_missing_one`.
- **`***` is not a value.** A masked round trip must not overwrite or move a stored
  credential: `masked_mcp_update_rejects_renamed_credential`,
  `update_agent_rejects_masked_round_trip_before_writing`.
- **A schedule tick runs only its own agent,** not every scheduled one:
  `test_filter_schedule_trigger_keeps_only_source_agent`.
- **Without Docker a host refuses runs unless opted in,** or agents run bare on it:
  `a_host_that_runs_workloads_keeps_its_provider_choice`.
- **A symlink is never imported as a credential,** or a sign-in reads another's:
  `a_planted_symlink_is_never_imported_as_the_credential`.
- **Instances can share a Docker daemon,** so a gateway is removed only if it is
  this one's: `a_disabled_gateway_is_removed_only_when_it_is_this_instances`.

## Do not

- Assume `AgentRunService::cancel_run` stops a run: it only writes the row, and
  `execute_run` does not read it back. `AutofixerService::cancel_run` does release.
- Register `AgentsPlugin` after the gateway; `startup/plugins.rs` in `temps-cli`
  says why application threads then fail closed.
- Lift `MAX_EPHEMERAL_*` or the forced `report` in `handlers/workflows.rs`.

## Testing

`cargo test --lib -p temps-ai-agents <name>`. `services/secret_service.rs` uses
`TestDatabase` and skips without a container runtime. Two `preview_gateway.rs`
tests use local Docker and skip without it or the node sandbox image:
`disable_removes_the_gateway_and_its_ingress_companion`,
`recreated_custom_gateway_preserves_db_owned_legacy_sandbox_network`.
`test_executor_happy_path_clones_pushes_creates_pr` is ignored as known broken.
CI: `docker-deployments` (`kind(=lib)`); no `tests/` binary or allow-list entry.
