// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Schema-level coverage for the ADR-113 identical builds migration.
//!
//! 1. **Existing projects keep building in parallel.** A project row written
//!    without the new columns must read `parallel` / `fingerprint`.
//! 2. **Only known values are stored**: the CHECK constraints refuse a mode
//!    or scope the binary does not understand.
//! 3. **One builder per group** is enforced by the database, not by the
//!    coordinator alone: the partial unique index must carry its predicate.
//! 4. **`down` is real** and `up` is replayable after it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use temps_migrations::{IdenticalBuildsMigration, Migrator};
use temps_test_containers::TestContainerExt;
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

const ONE_BUILDER_INDEX: &str = "identical_builds_one_builder_per_group";
const REUSABLE_INDEX: &str = "identical_builds_reusable";

async fn connect_with_retries(database_url: &str) -> anyhow::Result<DatabaseConnection> {
    let mut retries = 5;
    loop {
        match Database::connect(database_url).await {
            Ok(db) => return Ok(db),
            Err(error) if retries > 0 => {
                retries -= 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn scalar<C, T>(db: &C, sql: &str, column: &str) -> anyhow::Result<T>
where
    C: ConnectionTrait,
    T: sea_orm::TryGetable,
{
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            sql.to_string(),
        ))
        .await?
        .expect("query returns one row");
    Ok(row.try_get("", column)?)
}

async fn relation_exists<C: ConnectionTrait>(db: &C, name: &str) -> anyhow::Result<bool> {
    scalar(
        db,
        &format!("SELECT to_regclass('public.{name}') IS NOT NULL AS present"),
        "present",
    )
    .await
}

async fn project_columns<C: ConnectionTrait>(db: &C) -> anyhow::Result<i64> {
    scalar(
        db,
        "SELECT COUNT(*) AS n FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'projects' \
           AND column_name IN ('identical_build_mode', 'identical_build_scope')",
        "n",
    )
    .await
}

#[tokio::test]
async fn test_identical_builds_migration_defaults_constraints_and_reverses() -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        eprintln!("Skipping identical builds migration test: external database in use");
        return Ok(());
    }

    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
        // The real server logs readiness on stderr; the initdb one on stdout.
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .with_startup_timeout(std::time::Duration::from_secs(120))
        .with_test_labels()
        .start()
        .await
    {
        Ok(container) => container,
        Err(error) => {
            eprintln!("Skipping identical builds migration test: Docker unavailable: {error}");
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let database_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&database_url).await?;

    Migrator::up(&db, None).await?;

    // A project written the way every pre-ADR-113 caller wrote one.
    db.execute_unprepared(
        "INSERT INTO projects \
           (name, repo_name, repo_owner, directory, main_branch, preset, created_at, updated_at, slug) \
         VALUES ('Existing', 'app', 'example', '.', 'main', 'dockerfile', now(), now(), 'existing')",
    )
    .await?;
    let mode: String = scalar(
        &db,
        "SELECT identical_build_mode FROM projects WHERE slug = 'existing'",
        "identical_build_mode",
    )
    .await?;
    let scope: String = scalar(
        &db,
        "SELECT identical_build_scope FROM projects WHERE slug = 'existing'",
        "identical_build_scope",
    )
    .await?;
    assert_eq!(
        (mode.as_str(), scope.as_str()),
        ("parallel", "fingerprint"),
        "an existing project must keep building in parallel after the upgrade"
    );

    // Unknown values never reach the row.
    for update in [
        "UPDATE projects SET identical_build_mode = 'eager' WHERE slug = 'existing'",
        "UPDATE projects SET identical_build_scope = 'commit' WHERE slug = 'existing'",
    ] {
        assert!(
            db.execute_unprepared(update).await.is_err(),
            "the CHECK constraint must refuse: {update}"
        );
    }
    db.execute_unprepared(
        "UPDATE projects SET identical_build_mode = 'share', identical_build_scope = 'source' \
         WHERE slug = 'existing'",
    )
    .await?;

    // The rendezvous index is what makes "one builder per group" hold across
    // processes; without its predicate it would serialise finished rows too.
    let one_builder: String = scalar(
        &db,
        &format!("SELECT pg_get_indexdef('{ONE_BUILDER_INDEX}'::regclass) AS definition"),
        "definition",
    )
    .await?;
    assert!(
        one_builder.contains("UNIQUE")
            && one_builder.contains("queue_key")
            && one_builder.contains("'building'"),
        "one-builder index must be unique on queue_key while building; was: {one_builder}"
    );
    let reusable: String = scalar(
        &db,
        &format!("SELECT pg_get_indexdef('{REUSABLE_INDEX}'::regclass) AS definition"),
        "definition",
    )
    .await?;
    assert!(
        reusable.contains("'succeeded'") && reusable.contains("'share'"),
        "reusable index must cover only succeeded share builds; was: {reusable}"
    );

    // Rolled back in a transaction so `down` runs against the row above.
    let transaction = db.begin().await?;
    let manager = SchemaManager::new(&transaction);
    IdenticalBuildsMigration.down(&manager).await?;
    assert!(
        !relation_exists(&transaction, "identical_builds").await?,
        "down must drop the coordination table"
    );
    assert_eq!(
        project_columns(&transaction).await?,
        0,
        "down must drop both project columns"
    );

    // An operator retrying an upgrade replays up; it must be idempotent.
    IdenticalBuildsMigration.up(&manager).await?;
    IdenticalBuildsMigration.up(&manager).await?;
    assert!(relation_exists(&transaction, "identical_builds").await?);
    assert!(relation_exists(&transaction, ONE_BUILDER_INDEX).await?);
    assert_eq!(project_columns(&transaction).await?, 2);
    let mode_after: String = scalar(
        &transaction,
        "SELECT identical_build_mode FROM projects WHERE slug = 'existing'",
        "identical_build_mode",
    )
    .await?;
    assert_eq!(
        mode_after, "parallel",
        "re-applied up starts from the default"
    );
    transaction.rollback().await?;

    Ok(())
}
