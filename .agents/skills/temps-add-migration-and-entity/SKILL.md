---
name: temps-add-migration-and-entity
description: Add a database migration in crates/temps-migrations and the matching Sea-ORM entity in crates/temps-entities - file naming, registration in Migrator, up/down, TimescaleDB and Postgres specifics, encrypted columns and migration tests. Use whenever a change adds or alters a table, column, index, constraint or hypertable, or adds a per-record setting.
---
<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Add a migration and its entity

A schema change in Temps is always three pieces in one PR: a new migration file,
its registration in `Migrator`, and the entity (new or changed) that reads the
table. A test proves the migration applies to an existing database and, where it
matters, that `down` restores the previous state.

Read first: `AGENTS.md` (NEVER list, "Configuration is a column", "Data, logging and secrets"), `REVIEW.md` → "Migrations and the
database".

## 1. Decide what the change is

- **Runtime configuration is a column, never an env var.** A new knob belongs on
  the row it configures (`oidc_providers.trust_idp_email`,
  `project_analytics_settings.exclude_datacenter_traffic`), so it can be changed
  per record through the API, gets audit logging, and needs no restart. Env vars
  are only for bootstrap values needed before the database exists.
- **Sensitive values are encrypted at rest.** Name the column `<thing>_encrypted`
  (`TEXT`), write it with `EncryptionService::encrypt_string` and read it with
  `decrypt_string` (`crates/temps-core/src/encryption.rs`). Responses expose a
  boolean such as `has_secret`, never the value. Worked example:
  `crates/temps-revenue/src/service/integration.rs`.
- **A shipped migration is never edited.** Once a migration has been released,
  its name is recorded in `seaql_migrations` on users' databases and it never runs
  again. Fix or extend it with a new migration.
- **Stay compatible with the previous binary.** `crates/temps-database/src/schema_guard.rs`
  lets an operator roll the binary back after an upgrade with the newer schema in
  place. Prefer additive changes (new nullable or defaulted column, new table).
  Only drop or rename a column after the code has stopped reading it in an earlier
  release; see `m20261105_000002_drop_webhook_deliveries_response_body.rs`.

## 2. Create the migration file

Location: `crates/temps-migrations/src/migration/`.

Name: `mYYYYMMDD_NNNNNN_<snake_case_description>.rs`. Use today's date and the
next free sequence number for that date. `DeriveMigrationName` turns the file name
into the persisted migration name, so it must be unique.

Shape (mirrors `m20261104_000002_create_project_analytics_settings.rs`):

```rust
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Why this table/column exists, what the default means for existing rows,
//! and whether a backfill is needed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE things ADD COLUMN IF NOT EXISTS enabled boolean NOT NULL DEFAULT true",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE things DROP COLUMN IF EXISTS enabled")
            .await?;
        Ok(())
    }
}
```

Rules:

- Raw SQL through `execute_unprepared` is the dominant style; keep statements
  idempotent (`IF NOT EXISTS`, `IF EXISTS`).
- Always write a real `down`. If the data cannot be restored, say so in the module
  doc and make `down` restore the shape (as the drop-column example above does).
  If `down` would destroy live user data, refuse with an error instead (see the
  Bunny delivery downgrade test in `crates/temps-migrations/tests/migration_tests.rs`).
- Timestamps are `timestamptz`, normally `NOT NULL DEFAULT now()`.
- Foreign keys state their `ON DELETE` behaviour explicitly.
- The module doc comment explains the *why*: defaults for existing rows, backfill,
  and why a table rather than a column (or the reverse).

### Postgres and TimescaleDB specifics

- **Hypertables**: `SELECT create_hypertable('<table>', '<time column>', ...,
  if_not_exists => TRUE)`. Example: `m20260225_000001_create_otel_tables.rs`.
- **`time_bucket`**: never cast it at the same query level as `GROUP BY`; bucket
  in a subquery.
- **Large or hot tables**: a plain `CREATE INDEX` blocks writes for the whole
  scan. Build such indexes `CONCURRENTLY` after boot through
  `run_post_migration_indexes` in `crates/temps-database/src/connection.rs`
  (see `REFERENCE_INDEXES`, and `FK_INDEXES` exported from
  `m20261105_000001_project_data_purges.rs`), not inside the migration.
- **Backfills**: add the column first, then backfill in committed batches. Do not
  rewrite every row while `ALTER TABLE` still holds its exclusive lock.
- **Uniqueness claims** (hostnames, ports, slugs) are enforced with a unique
  index, not with read-then-write in the service.

## 3. Register it

Two edits in `crates/temps-migrations/src/migration/mod.rs`:

1. Declare the module next to the others: `mod mYYYYMMDD_NNNNNN_description;`
   (`pub mod` only if another crate or an integration test needs its SQL or
   struct; then also `pub use` it from `crates/temps-migrations/src/lib.rs`, as
   `NormalizedManagedDomainIndexMigration` is).
2. Append `Box::new(mYYYYMMDD_NNNNNN_description::Migration),` at the **end** of
   the `vec![...]` in `impl MigratorTrait for Migrator`, with a one-line comment.

Position in that list is upgrade history. Never reorder or insert in the middle,
even if the date sorts earlier. The `registry_tests` module at the bottom of
`mod.rs` asserts names are unique and that known shipped migrations keep their
relative order.

## 4. Add or update the entity

Location: `crates/temps-entities/src/<table_name>.rs`, declared with
`pub mod <table_name>;` in `crates/temps-entities/src/lib.rs` (alphabetical).
Adding it to `crates/temps-entities/src/prelude.rs` is optional.

Mirror `crates/temps-entities/src/project_analytics_settings.rs`:

- `#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]`
  and `#[sea_orm(table_name = "...")]`.
- Timestamps are `temps_core::DBDateTime` (an alias for `chrono::DateTime<Utc>`,
  serialised with a `Z` suffix). Never `NaiveDateTime`.
- Nullable columns are `Option<T>`; column types match the SQL exactly. A
  mismatch fails at decode time, not at compile time.
- `Relation` + `Related` for every foreign key you will join on (JOINs, not N+1).
- `ActiveModelBehavior::before_save` sets `created_at`/`updated_at` when the
  table has them.
- Doc-comment each setting column with what it does and its default.

When altering an existing table, update its entity in the same PR and grep for
struct literals of that `Model` (tests build them by hand) so they still compile.

## 5. Test it

Every schema change ships a test (`AGENTS.md`, `REVIEW.md`).

- **Registry**: `cargo test --lib -p temps-migrations` runs `registry_tests`.
- **Up/down against a real database**: add a `#[tokio::test]` to
  `crates/temps-migrations/tests/migration_tests.rs`. Follow its pattern: start
  `timescale/timescaledb-ha:pg18` through `testcontainers`, skip with a message
  when Docker is unavailable or `external_db_configured()`, migrate to just before
  your migration, insert representative rows, run `Migrator::up(&db, Some(1))`,
  assert, then `Migrator::down` and assert again. Use the `steps_back_to(name)`
  helper rather than a hard-coded step count. Never mark it `#[ignore]`.
- **Service-level tests on the new schema**: `TestDatabase::with_migrations()`
  from `crates/temps-database/src/test_utils.rs`, skipping when it returns `Err`
  (see `try_test_db` in `crates/temps-revenue/tests/end_to_end.rs`).
- When you change a column default, grep the existing tests for the old default.

## Verification

```bash
cargo check --lib -p temps-migrations
cargo check --lib -p temps-entities
cargo test --lib -p temps-migrations
cargo test -p temps-migrations --test migration_tests <your_test_name>
cargo clippy -p temps-migrations -p temps-entities --all-targets -- -D warnings
python3 scripts/rule_drift.py
python3 scripts/source_attribution.py check
```

Also `cargo check --lib` for every crate whose code uses the changed entity.
Run the migration test with Docker available: a skipped test proves nothing.

## Commit

- New `.rs` files carry the SPDX header (`python3 scripts/source_attribution.py
  annotate <file>`).
- Conventional Commits with DCO: `git commit -s -m "feat(<area>): add <column> to <table>"`.
- A new entity column usually means an API change: follow the `temps-add-handler`
  skill to expose it and regenerate the OpenAPI clients.

## Checklist

- [ ] New file, never an edit to a shipped migration
- [ ] `mod` declared and `Box::new(..)` appended at the end of `Migrator::migrations()`
- [ ] Working `down`, or an explicit refusal when it would destroy user data
- [ ] Additive and compatible with the previous binary
- [ ] No blocking index build or locked backfill on a large table
- [ ] Config is a column; secrets are `*_encrypted` via `EncryptionService`
- [ ] Entity uses `DBDateTime`, `Option<T>` for nullable, relations for joins
- [ ] Migration test added and run with Docker available
