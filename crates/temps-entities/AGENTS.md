# temps-entities

The Sea-ORM models of the PostgreSQL schema and the types stored in their columns.
These rules add to [`AGENTS.md`](../../AGENTS.md). Recipe: the
[`temps-add-migration-and-entity`](../../.agents/skills/temps-add-migration-and-entity/SKILL.md)
skill. The other half of a schema change is
[`temps-migrations`](../temps-migrations/AGENTS.md).

## Owns

One module per table, stored enums and typed JSON columns, and query helpers and
pure rules several crates share. It does not own:

- The schema: `temps-migrations`, which depends on this crate.
- API shapes: DTOs live with handlers (`S3SourceResponse` in `temps-backup`).
- Domain types it stores: `SandboxEgressPolicy` (`temps-workload`), `AccessPolicy`
  and `EndpointTarget` (`temps-workload-ingress`), wrapped here as
  `StoredEgressPolicy`, `StoredPolicy` and `StoredTarget`.

## Entry points

- `src/<table>.rs`, declared in `src/lib.rs`; `project_analytics_settings.rs` is
  the shape the skill mirrors.
- `types.rs`, `source_type.rs`, `preset.rs`, `deployment_config.rs`: stored types.
- `node_resource_settings.rs`, `node_network_policy_modes.rs`,
  `environment_workload_isolation.rs`: narrow views, a second `Model` over `nodes`
  or `environments` with only the columns one path uses.
- `s3_sources.rs`: `insert_encrypted`, `update_encrypted`, `decrypt_session_token`.
- Locks and allocators: `claim_subdomain`, `lock_hostname_claim`,
  `lock_node_placements`, `allocate_deployment_slug`.

## Invariants

- **A `Model` must match its table, and nothing here checks that.** With no
  database tests in the crate, a wrong type compiles and fails at decode time in
  whichever crate reads the row.
- **Text enum values match the migration's `CHECK`.** A new variant needs a new
  migration widening it, or the write fails; a stored value outside the enum fails
  the decode for the whole result set
  (`m20260901_000005_add_cloud_telemetry_write_mode.rs`). Tests compare with literal
  values, not the SQL: `display_matches_the_stored_column_values`,
  `modes_serialise_as_their_column_values`.
- **Integer enums are positional.** `JobStatus` backs `deployment_jobs.status`
  through `num_value`; renumbering reinterprets existing rows.
- **Defaults are safety decisions.** `the_default_is_local`
  (`cloud_telemetry_write_mode.rs`) keeps an upgrade storing spans locally.
  `the_default_policy_is_the_column_default` (`sandboxes.rs`) pins
  `StoredEgressPolicy::default()` to the `DEFAULT` of
  `m20261024_000011_sandbox_egress_policy`.
- **JSON types read rows already stored.** A new required field fails the decode;
  use `#[serde(default)]`. Pinned by
  `preset_info_deserializes_pre_existing_cache_rows_missing_the_field` and
  `test_legacy_nixpacks_config_defaults_to_auto_detection`.
- **`ComposeSecurityPolicy` fails closed** on an unknown check
  (`exceptions_round_trip_and_unknown_checks_fail_closed`).
- **Lock helpers hold only inside a transaction.** `lock_node_placements` and
  `lock_hostname_claim` accept any connection, and their comments say so.

## Do not

- Depend on a crate that depends on this one, including `temps-migrations`: Cargo
  rejects the cycle. `temps-workload` and `temps-workload-ingress` depend on no
  Temps crate. The second sits above this layer and is listed under "Edges against
  the layer order" in [`crates.md`](../../docs/architecture/crates.md), which says
  to cut such edges rather than add more.
- Add a narrow view's columns to `nodes::Model` or `environments::Model`; the
  module comments say this would change every place that builds the full model.
- Add a secret column that serializes. Use `#[serde(skip_serializing)]`, and a
  hand-written `Debug` if the derived one would print it;
  `encrypted_api_key_is_not_exposed_by_debug_or_json` pins both.
- Return a secret-holding `Model` from a handler. `nodes::Model`,
  `ai_provider_keys::Model` and `s3_sources::Model` lack `skip_serializing` there.

## Testing

`cargo test --lib -p temps-entities <name>`. Tests are plain `#[test]`s in the
module they cover and need no database or Docker; the crate has no `tests/`
directory or `[dev-dependencies]`. A helper's database behaviour is tested with
its caller: `concurrent_allocations_for_one_project_get_distinct_slugs`
(`temps-deployments`). CI: `unit-b` runs this package with `kind(=lib)`.
