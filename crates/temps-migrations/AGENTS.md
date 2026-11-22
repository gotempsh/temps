# temps-migrations

The PostgreSQL migration history and its generated `Migrator`. These rules add to
[`AGENTS.md`](../../AGENTS.md). Recipe: the
[`temps-add-migration-and-entity`](../../.agents/skills/temps-add-migration-and-entity/SKILL.md)
skill. The other half of a schema change is
[`temps-entities`](../temps-entities/AGENTS.md).

## Owns

Every PostgreSQL and TimescaleDB schema change and its run order, plus the SQL
other crates need from a migration. It does not own:

- Running migrations, the schema guard and `CONCURRENTLY` index builds:
  `temps-schema` (`run_migrations`, `check_schema_compatibility`,
  `run_post_migration_indexes`). `MIGRATION_TIMEOUT` there caps a run at 600 s.
- ClickHouse schemas: `temps-proxy`, `temps-metrics`, `temps-otel`,
  `temps-analytics-backend` and `temps-log-aggregator` each apply their own
  `migrations/clickhouse/*.sql`.

## Entry points

- `src/migration/mYYYYMMDD_NNNNNN_<name>.rs`: one migration; the file registers it.
- `build.rs` writes `generated_migrations()` using `order` from
  `src/migration_order.rs`; `src/migration/mod.rs` `include!`s the result.
- `src/migration/order_exceptions.txt`: migrations placed out of file-name order.
- `tests/fixtures/migration_order.txt`: the shipped order, one name per line.

## Invariants

- **Run order is upgrade history.**
  `registered_order_starts_with_shipped_history_fixture` requires the generated
  list to begin with every fixture line, in order; a new migration may follow
  without a line. On "a shipped migration was removed", restore the file. On
  "migration order diverges", a new file sorts too early: rename it past the
  highest file name in `src/migration/`, which may differ from the fixture's last
  line that the message prints. Leave both lists alone.
- **The file name is the persisted name.** `DeriveMigrationName` records it in
  `seaql_migrations`; a rename leaves an unknown ledger entry and the migration
  pending again. `every_migration_file_is_registered` ties files to the list.
- **`src/migration/` holds only migrations.** `migration_name` fails the build on
  anything else (`migration_names_come_from_well_formed_rs_files_only`).
- **`src/migration_order.rs` uses only `std`.** `build.rs` compiles it by `#[path]`
  and the crate has no `[build-dependencies]`.
- **Each `Migrator::up` call is one PostgreSQL transaction**, whether `temps-schema`
  passes the whole batch or `Some(1)`, so `CONCURRENTLY` is illegal in a migration,
  as `m20260805_000001_index_normalized_managed_domains.rs` notes.
  `test_post_migration_indexes_cover_project_foreign_keys` (`temps-schema`) asserts
  these indexes are built after the migration.
- **All of history rolls back on an empty database** (`test_migration_down`). A
  `down` may still refuse to discard live data
  (`test_bunny_delivery_downgrade_preserves_active_profiles`).
- **Other crates pin these files.** `purged_tables_match_the_migration`
  (`temps-projects`) compares `PROJECT_DATA_PURGED_TABLES`, and
  `application_workspace_runtimes_resolve_to_managed_daemon_images`
  (`temps-sandbox`) reads two migration files by path.

## Do not

- Edit a fixture line or an exceptions block; each file's header forbids it.
- List this crate outside `[dev-dependencies]`, except in `temps-schema` and
  `temps-test-utils`. `crates/temps-schema/src/lib.rs` says why: a migration must
  not rebuild every crate that opens a connection.
- Use an entity `Model`, `Column` or enum in a migration: old migrations replay
  against the entities at that head. Only the two CIDR constants in
  `temps_entities::network_config` are imported; others copy the value under a
  `Kept in sync with` comment.
- Hard-code a `Migrator::down` step count; use `steps_back_to`.
- Run a cycle against a shared database; `start_pg_container!` guards against it.

## Testing

`cargo test --lib -p temps-migrations registry_tests` and `migration_order` need
no database. `cargo test --lib -p temps-migrations` adds the test modules inside
migration files: `MockDatabase`, or a container that skips when Docker is missing.
`cargo test -p temps-migrations --test migration_tests <name>` runs one of the
four TimescaleDB binaries in `tests/`. All but `test_pgvector_extension` pass early
when `TEMPS_TEST_DATABASE_URL` is non-empty (empty counts as unset). Most skip
without Docker, but 12 in `migration_tests.rs`, including `test_migration_up`,
panic. CI: the `migrations` group runs every target with that variable emptied.
