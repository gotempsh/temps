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

async fn insert_binding(
    txn: &DatabaseTransaction,
    project_id: i32,
    hostname: &str,
    custom_domain_id: i32,
) -> anyhow::Result<i32> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO domain_delivery_bindings
                 (hostname, project_id, environment_id, custom_domain_id, profile_id,
                  profile_source, dns_provider_id, zone, origin_target, record_type, proxied,
                  status)
             VALUES ($1, $2, 1, $3, 1, 'project', 1, 'example.test', 'origin.example.test',
                     'CNAME', true, 'applied')
             RETURNING id",
            [hostname.into(), project_id.into(), custom_domain_id.into()],
        ))
        .await?
        .expect("insert returns the row");
    Ok(row.try_get("", "id")?)
}

async fn binding_target(txn: &DatabaseTransaction, binding_id: i32) -> anyhow::Result<i32> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT custom_domain_id FROM domain_delivery_bindings WHERE id = $1",
            [binding_id.into()],
        ))
        .await?
        .expect("no binding is deleted");
    Ok(row.try_get("", "custom_domain_id")?)
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
    // Delivery on a row that loses to one serving traffic: in the same project and
    // environment it can move, in another project it can't.
    let same_project_kept = insert_domain(&txn, 5, "moves.example.test", "active", 1).await?;
    let same_project_loser = insert_domain(&txn, 5, "moves.example.test", "pending", 3).await?;
    let moved_binding = insert_binding(&txn, 5, "moves.example.test", same_project_loser).await?;
    let other_project_kept = insert_domain(&txn, 6, "stays.example.test", "active", 1).await?;
    let other_project_loser = insert_domain(&txn, 7, "stays.example.test", "pending", 3).await?;
    let kept_binding = insert_binding(&txn, 7, "stays.example.test", other_project_loser).await?;

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

    assert_eq!(
        binding_target(&txn, moved_binding).await?,
        same_project_kept,
        "the winner serves this hostname for the same environment"
    );
    let (_, _, message) = domain_of(&txn, same_project_loser).await?;
    assert!(message.contains("can be deleted"), "{message}");
    assert!(!message.contains("delivery"), "{message}");

    assert_eq!(
        binding_target(&txn, kept_binding).await?,
        other_project_loser,
        "delivery for another project can't change hands in a migration"
    );
    let (domain, _, message) = domain_of(&txn, other_project_loser).await?;
    assert_eq!(domain, format!("duplicate-{other_project_loser}.invalid"));
    assert!(message.contains("stays.example.test"), "{message}");
    assert!(
        message.contains("Remove that delivery binding"),
        "{message}"
    );
    assert!(
        message.contains(&other_project_kept.to_string()),
        "{message}"
    );

    let duplicate = insert_domain(&txn, 4, "App.Example.Test", "pending", 0).await;
    let error = duplicate.expect_err("the index refuses another copy, in any case");
    assert!(error.to_string().contains("unique"), "{error}");

    txn.rollback().await?;
    Ok(())
}
