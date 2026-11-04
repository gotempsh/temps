# temps-providers-core

The contract and shared machinery beneath the managed-service engines. Adds to
[`AGENTS.md`](../../AGENTS.md).

## Owns

Dependencies run one way: [`temps-providers`](../temps-providers/AGENTS.md) → the
five `temps-providers-<engine>` crates → this crate → `temps-service-protocol`.

- `ExternalService` and its types, `EngineError`, `ExternalServiceError`.
- What engines share: container replacement, restore staging and fencing, the
  restore image allowlist, S3 backend selection.
- The engine-neutral half of query, data import, readiness, the sandbox namespace
  and the workload plane.

Not here: engine code and database drivers (the engine crates); choosing an
engine, the manager, handlers and import run bookkeeping (`temps-providers`);
`ServiceConfig`, `ServiceType` and `NodeServiceRuntime` (`temps-service-protocol`);
building `RestoreContext` and implementing `RestoreGate` (`temps-backup`).

## Entry points

- `src/externalsvc/mod.rs`: `ExternalService`, `RestoreContext`, `S3Credentials`.
- `src/externalsvc/error.rs`, `src/service_errors.rs`: the two error enums.
- `src/externalsvc/recreate_guard.rs`: `recreate_preserving_data`, `check_storage`.
- `src/query/`: `DataSource`, `BoundedRows`. `QueryService` in `temps-providers`
  picks the source; `QueryRegistry` has no caller outside its module.
- `src/data_import/`: `DataImportEngine`, `runner`, `source`.
- `src/readiness.rs`: `ReadinessTarget`, `drive_readiness`.
- `src/workload/`: `DatabaseWorkloadPlan`, `enforced_create`, `changes_fingerprint`.

## Invariants

- A replacement container must prove it holds the old data, or an empty `initdb`
  reports success and the next backup captures nothing: `a_differently_named_volume_is_refused`,
  `a_fresh_initdb_never_passes_verification`, `a_replacement_on_other_storage_is_rolled_back`.
- `EngineError` constructors redact, or driver output leaks passwords into API
  responses: `no_constructor_lets_a_connection_string_or_password_pair_through`.
- Docker's name filter matches substrings; without `exact_named_container` one
  service acts on another's container. Pinned in `temps-providers` by
  `docker_substring_name_match_is_not_treated_as_the_requested_container`.
- `BoundedRows` is the query memory bound: `oversized_first_row_is_rejected`,
  `later_row_over_page_budget_truncates_without_adding_it`.
- `enforced_create` returns `None` for a container outside the enforce table, so a
  default install sends each engine's own request (golden fixtures in `temps-providers`).
- Admission refuses what a spec may not carry: `a_host_path_volume_is_refused_at_admission`,
  `a_command_only_secret_becomes_a_placeholder_and_never_a_variable`.
- A readiness write cannot overwrite a newer start, which would report the wrong
  attempt's verdict: `a_stale_attempt_cannot_write_over_a_newer_start`.
- Import sources cannot reach internal addresses or echo a password:
  `ssrf_guard_refuses_private_loopback_and_metadata_sources`,
  `refuses_malformed_input_without_echoing_the_password`.
- A restore to a new service gives its image the source's root credentials:
  `lookalike_repositories_are_rejected` guards that allowlist.

## Do not

- Add an engine crate, `temps-providers` or a database driver as a normal
  dependency. The first two are Cargo cycles; drivers are dev-only, for tests.
- Enable `test-support` outside dev-dependencies (comment in `Cargo.toml`).
- Decrypt `RestoreContext::s3_source`: it holds plaintext, so decrypting fails.
- Read `docker_resource_names() == None` as "nothing exists", or replace a
  container when `recreate_plan` returns `None`. Refuse instead.
- Buffer a restore object in memory (`restore_staging`) or rows ahead of `BoundedRows`.
- Make the readiness gate stop or delete anything (`readiness.rs` header).

## Testing

`cargo test --lib -p temps-providers-core <name>`; add `--features docker-tests`
for `run_exec_returns_for_a_command_with_no_stdout`, `test_s3_container_lifecycle`
and the `S3TestContainer` fixture.

- Other Docker tests need no feature and skip when no daemon answers:
  `recreate_guard::docker_tests`, `fence_stops_running_helpers_and_spares_the_service`.
- `TestDatabase` tests in `readiness.rs` skip only on `is_container_runtime_unavailable`.
- CI: `unit-a` runs the lib tests without the feature, `docker-providers` with it.
  `query::` tests run in `docker-deployments` instead, since that filter covers
  every `temps-providers*` crate. `postgres-upgrades` and `docker-backups` select
  nothing here. There are no integration binaries and no allow-list entry.
