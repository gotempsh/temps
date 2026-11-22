# temps-providers

The manager, API and background work above the managed-service engines. Adds to
[`AGENTS.md`](../../AGENTS.md).

## Owns

- `ExternalServiceManager`: service rows, encrypted parameters, lifecycle, clusters,
  project links, env vars, routing to worker agents, choosing an engine.
- The HTTP surface, `ProvidersPlugin` and the role reconcilers, DNS sweep and
  workload shadow loop it starts.
- `QueryService`, `DataImportService`, `PostgresUpgradeService`, sandbox access,
  parameter validation, KV and Blob naming, `ProvidersNodeRuntime`.

Not here: engine code (`temps-providers-<engine>`, re-exported as
`temps_providers::externalsvc::postgres` and so on); `ExternalService` and what
engines call ([`temps-providers-core`](../temps-providers-core/AGENTS.md), also
re-exported); wire types and `NodeServiceRuntime` (`temps-service-protocol`); the
agent's HTTP server (`temps-node-agent`); restore orchestration (`temps-backup`);
the KV and Blob plugins (`temps-kv`, `temps-blob`); DNS storage (`temps-dns`).

## Entry points

- `src/plugin.rs`: what is registered and started.
- `src/services/mod.rs`: `ExternalServiceManager`, `create_service_instance`,
  `resolve_service_instance`. `src/services/manager/` adds one `impl` per concern.
- `src/services/workload_ops.rs`: `recreate_from_workload_spec`.
- `src/handlers/`: `handlers.rs` holds `external_service_problem` and `ExternalServiceApiDoc`.
- `src/externalsvc/mod.rs`: `probe_service_health`, `provision_runtime_environment`.
- `src/query_service.rs`, `src/data_import/service.rs`, `src/parameter_strategies.rs`.
- `src/sandbox_engines.rs` (`ENGINES`), `src/node_runtime.rs`, `src/remote_service_client.rs`.

## Invariants

- An S3 or Blob service runs the engine in its stored parameters, or a backup acts
  on the wrong container: `a_registered_garage_service_resolves_to_the_garage_engine`.
- A guard error from `start` is final; re-initialising recreates over a set-aside
  original: `a_failed_start_after_a_guard_error_is_never_retried_by_reinitialising`.
- A worker re-admits every plan, or an edited request creates a container nobody
  admitted: `a_plan_edited_in_transit_is_refused_on_the_worker`.
- Probes accept only a loopback host, or a stored `host` becomes an SSRF path:
  `health_probe_still_rejects_caller_supplied_non_loopback_hosts`.
- Data browser dials are vetted, rows clamped, errors echo nothing back, or a query
  reaches internal hosts, OOMs the host or leaks data: `vet_query_target_rejects_internal_literals`,
  `effective_row_limit_clamps_every_caller_not_just_the_agent`,
  `query_error_problem_does_not_echo_backend_diagnostics`.
- A credential reveal fails when its audit write fails, unlike the root's
  degrade rule: `credential_reveal_fails_closed_when_audit_write_fails`.
- A metrics handler must also be listed in `ExternalServiceApiDoc`, or no client
  binding is generated: `served_openapi_doc_declares_every_metrics_operation`.
- A sandbox type with no engine is left out, never given deployment credentials:
  `a_type_with_no_engine_is_left_out`, `no_service_type_is_claimed_by_two_engines`.
- `tests/fixtures/golden/` is each engine's create request (`golden_container_create`).
  A diff changes what production sends; review it before `TEMPS_UPDATE_GOLDEN=1`.

## Do not

- Depend on this crate from `temps-node-agent` outside dev-dependencies. Add the
  operation to `NodeServiceRuntime` and implement it in `src/node_runtime.rs`.
- Put something an engine must call here: engines cannot depend on this crate.
- Call `ExternalServiceManager::new` in a background task. Clone it, or
  `stop_role_reconciler` cannot reach what that task spawned (`src/services/mod.rs`).
- Reach Docker before `local_workloads_enabled`: the operator should see the policy
  error, not a capability error (comment on `require_docker`).
- Copy the `state.db` reads in `metrics_handlers.rs`; they are baselined exceptions.

## Testing

`cargo test --lib -p temps-providers <name>`; `--features docker-tests` turns the
feature on in core and every engine crate too. Orchestrator tests take minutes:
`cargo test --lib -p temps-providers --features docker-tests orchestrator_ -- --nocapture --test-threads=1`.

- `setup_test_manager_or_skip!` skips only on `is_container_runtime_unavailable`.
- Integration binaries need no feature. `golden_container_create` uses a fake Engine
  API, no Docker. The WAL and binlog binaries skip without Docker.
  `role_reconciler_integration` also skips if `TEMPS_TEST_DATABASE_URL` is non-empty.
- CI: `unit-a` runs the lib tests without the feature; `docker-providers` with it,
  minus `orchestrator_`, which run serially in `postgres-upgrades`. `docker-deployments`
  runs the integration binaries except `role_reconciler_integration`, which runs in
  `migrations`. `docker-backups` selects nothing here. No allow-list entry.
