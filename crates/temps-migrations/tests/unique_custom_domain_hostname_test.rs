// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test helpers: panicking is how a test fails"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration test: reads fixture configuration from the environment"
)]
use sea_orm::{ConnectionTrait, Database, DatabaseTransaction, Statement, TransactionTrait};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use temps_migrations::{Migrator, UniqueCustomDomainHostnameMigration};
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

async fn insert_domain(
    txn: &DatabaseTransaction,
    project_id: i32,
    domain: &str,
    status: &str,
    age_days: i32,
) -> anyhow::Result<i32> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO project_custom_domains
                 (project_id, environment_id, domain, status, created_at, updated_at)
             VALUES ($1, 1, $2, $3, now() - make_interval(days => $4), now())
             RETURNING id",
            [
                project_id.into(),
                domain.into(),
                status.into(),
                age_days.into(),
            ],
        ))
        .await?
        .expect("insert returns the row");
    Ok(row.try_get("", "id")?)
}

async fn domain_of(txn: &DatabaseTransaction, id: i32) -> anyhow::Result<(String, String, String)> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT domain, status, coalesce(message, '') AS message
             FROM project_custom_domains WHERE id = $1",
            [id.into()],
        ))
        .await?
        .expect("no row is deleted");
    Ok((
        row.try_get("", "domain")?,
        row.try_get("", "status")?,
        row.try_get("", "message")?,
    ))
}

#[tokio::test]
async fn existing_duplicates_keep_one_winner_and_lose_nothing() -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        eprintln!("Skipping unique custom domain test: external database in use");
        return Ok(());
    }

    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
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
        .start()
        .await
    {
        Ok(container) => container,
        Err(error) => {
            eprintln!("Skipping unique custom domain test: Docker unavailable: {error}");
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let database_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = Database::connect(&database_url).await?;
    Migrator::up(&db, None).await?;

    let txn = db.begin().await?;
    let manager = SchemaManager::new(&txn);
    UniqueCustomDomainHostnameMigration.down(&manager).await?;
    // The rows only need to exist as duplicates: skip the foreign keys rather
    // than build a project and environment for each.
    txn.execute_unprepared("SET LOCAL session_replication_role = replica")
        .await?;

    let older_pending = insert_domain(&txn, 1, "app.example.test", "pending", 9).await?;
    let active = insert_domain(&txn, 2, "APP.example.test", "active", 1).await?;
    let newer_pending = insert_domain(&txn, 3, "app.example.test", "pending", 0).await?;
    let oldest_of_two = insert_domain(&txn, 1, "other.example.test", "pending", 5).await?;
    let newest_of_two = insert_domain(&txn, 1, "other.example.test", "pending", 2).await?;
    let unrelated = insert_domain(&txn, 1, "unique.example.test", "pending", 2).await?;

    UniqueCustomDomainHostnameMigration.up(&manager).await?;
    // Safe to run again once the duplicates are gone.
    UniqueCustomDomainHostnameMigration.up(&manager).await?;

    assert_eq!(domain_of(&txn, active).await?.0, "APP.example.test");
    assert_eq!(
        domain_of(&txn, oldest_of_two).await?.0,
        "other.example.test"
    );
    assert_eq!(domain_of(&txn, unrelated).await?.0, "unique.example.test");

    for (loser, hostname, kept) in [
        (older_pending, "app.example.test", active),
        (newer_pending, "app.example.test", active),
        (newest_of_two, "other.example.test", oldest_of_two),
    ] {
        let (domain, status, message) = domain_of(&txn, loser).await?;
        assert_eq!(domain, format!("duplicate-{loser}.invalid"));
        assert_eq!(status, "failed");
        assert!(message.contains(hostname), "{message}");
        assert!(message.contains(&kept.to_string()), "{message}");
    }

    let duplicate = insert_domain(&txn, 4, "App.Example.Test", "pending", 0).await;
    let error = duplicate.expect_err("the index refuses another copy, in any case");
    assert!(error.to_string().contains("unique"), "{error}");

    txn.rollback().await?;
    Ok(())
}
