# temps-core

The types and contracts nearly every crate builds on; a change here rebuilds
most of the workspace. These rules add to the root [`AGENTS.md`](../../AGENTS.md).

## Owns

- The plugin system, `Problem` responses, `RetryConfig`, encryption, request
  context, `Job`, the `.temps.yaml` format (`repo_config.rs`), and seams: a trait
  here, implemented in a crate that depends on this one.
- What the control plane and `temps-node-agent` must agree on: `gate.rs`,
  `gate_access.rs`, `worker_ingress.rs`, `tailnet.rs`, `node_pki.rs`.

Not here: database access (no `sea-orm` dependency; `temps-database`,
`temps-entities`); `RequireAuth`, `permission_guard!` (`temps-authz`);
`AuthMiddleware` (`temps-auth`); `ConfigService` (`temps-config`); `AuditService`
(`temps-audit`); queue implementations (`temps-queue`); plugin hosting and actor
tokens (`temps-external-plugins`); plugin order and router assembly (`temps-cli`,
`src/commands/serve/console/startup/`).

## Entry points

- `src/plugin.rs`: `TempsPlugin`, `PluginManager::initialize_plugins`,
  `build_split_application`, `build_unified_openapi`, `PluginRoutes::with_override`.
- `src/problemdetails/mod.rs`, `src/error_builder.rs`, `src/retry.rs`,
  `src/jobs.rs` (`Job`, `JobQueue`), `src/docker_handle.rs` (`DockerHandle`).
- Slot seams, for consumers built before plugins register: `project_ip_gate.rs`,
  `request_policy_gate.rs`, `retention.rs`, `dns_automation.rs`, `AuditLoggerSlot`.
  Registry seams: `project_access.rs`, `sensitive_action.rs`, `telemetry.rs`,
  `self_update.rs`, `route_table.rs`, `on_demand.rs`, `project_deletion.rs`.
- `build.rs` and `src/release_manifest.rs` generate the `release_images` constants
  from `TEMPS_RELEASE_IMAGE_MANIFEST`, set by `release.yml`; without it they are `None`.

## Invariants

- **Plugin phases.** Every `register_services`, `verify_required_services`, then
  every `initialize_plugin_services`. `require_service` panics on a missing service,
  so a provider used during `register_services` must be registered earlier.
- **Middleware order.** `collect_middleware` sorts by ascending priority and
  `apply_middleware_to_router` layers in that order, so the highest number is
  outermost and runs first, the reverse of what the `MiddlewarePriority` docs say.
  `RequestMetadataMiddleware` (100) must run before `AuthMiddleware` (0), or
  permission-denial records lose IP and user agent. No test pins this order.
- **Listeners.** Middleware without `apply_to_public` stays off the public router
  (`admin_only_middleware_does_not_apply_to_public_router`). A route override stays
  on its listener (`override_does_not_leak_across_admin_public_listeners`) and inside
  the middleware stack, or it bypasses auth (`override_runs_through_admin_middleware_stack`).
- **Proxy gate slots fail closed** until `set` or `finish_registration`, and the
  first `set` wins, so the open default cannot serve past a restrictive gate:
  `registration_race_never_exposes_open_gate_after_restrictive_gate_claims_slot`.
  `DnsAutomationGateSlot` denies until claimed: `unclaimed_slot_denies_automation`.
- **Stored ciphertext stays readable**, or encrypted rows and live sessions are
  lost: `test_decrypts_pre_upgrade_aes_gcm_payload`,
  `test_decrypts_values_minted_under_the_legacy_raw_key`.
- **`Job` payloads cross versions.** `DurableReceiver::claim` in `temps-queue`
  settles a payload it cannot parse as failed, so the job never runs. New fields
  take `#[serde(default)]`: `test_git_push_event_job_missing_recovery_source_defaults_to_none`.

## Do not

- Add a workspace crate to `[dependencies]`; it has none. Add a trait here and
  implement it above, as `route_table.rs` does for `temps-routes`.
- `require_service::<bollard::Docker>()`. `DockerHandle` is always registered; call
  `require()` where the daemon is needed and map `DockerUnavailable` to its `Problem`.
- Use `RetryConfig::retry` when an error can be permanent: it retries every `Err`.
  Use `retry_if` (`test_retry_if_returns_non_retryable_errors_at_once`).
- Register one type twice: `ServiceRegistry::register` overwrites silently. Shared
  lists use `ProjectDeletionHooks::get_or_register`.
- Start work in `configure_routes`, `configure_public_routes` or `configure_middleware`:
  `build_unified_openapi` and `get_middleware` call them again.
- Await or lock in `ProjectIpGate`, `RequestPolicyGate` or `RetentionResolver` reads.
- Import beyond `[build-dependencies]` in `release_manifest.rs`; `build.rs` compiles it.
- Lock a new dependent of `serde_yaml`, `bincode` or `ttf-parser` (`lib.rs` guard tests).

## Testing

`cargo test --lib -p temps-core <name>`; `cargo test -p temps-core --test
error_builder_tests` for `tests/`. Nothing needs a database or Docker. In CI, library
tests run in `unit-a` and `tests/*.rs` in `unit-integration`. No group runs doc tests,
and `compiled_constants_match_the_manifest_or_local_fallback` only takes its `None` branch.
