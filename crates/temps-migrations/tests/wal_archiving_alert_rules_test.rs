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
use temps_migrations::{Migrator, SeedPostgresWalArchivingAlertRulesMigration};
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

async fn insert_service(
    txn: &DatabaseTransaction,
    name: &str,
    service_type: &str,
    metrics_enabled: bool,
) -> anyhow::Result<i32> {
    let row = txn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO external_services (name, service_type, status, metrics_enabled, created_at, updated_at)
             VALUES ($1, $2, 'running', $3, now(), now())
             RETURNING id",
            [name.into(), service_type.into(), metrics_enabled.into()],
        ))
        .await?
        .expect("insert returns the row");
    Ok(row.try_get("", "id")?)
}

async fn insert_rule(
    txn: &DatabaseTransaction,
    service_id: i32,
    metric_name: &str,
    threshold: f64,
) -> anyhow::Result<()> {
    txn.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "INSERT INTO monitoring_alert_rules
             (service_id, name, metric_name, threshold, comparator, severity, for_duration_secs, enabled)
         VALUES ($1, $2, $2, $3, '>', 'warning', 60, true)",
        [service_id.into(), metric_name.into(), threshold.into()],
    ))
    .await?;
    Ok(())
}

/// `(metric_name, threshold, for_duration_secs)` of the service's rules on
/// the two archiving metrics, sorted by metric.
async fn archiving_rules(
    txn: &DatabaseTransaction,
    service_id: i32,
) -> anyhow::Result<Vec<(String, f64, i32)>> {
    let rows = txn
        .query_all(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT metric_name, threshold, for_duration_secs FROM monitoring_alert_rules
             WHERE service_id = $1
               AND metric_name IN ('pg.archiver_failing', 'pg.wal_dir_to_max_wal_size_ratio')
             ORDER BY metric_name",
            [service_id.into()],
        ))
        .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("", "metric_name")?,
                row.try_get("", "threshold")?,
                row.try_get("", "for_duration_secs")?,
            ))
        })
        .collect()
}

#[tokio::test]
async fn existing_postgres_services_with_metrics_get_the_archiving_alarms() -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        eprintln!("Skipping WAL archiving alert rules test: external database in use");
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
            eprintln!("Skipping WAL archiving alert rules test: Docker unavailable: {error}");
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
    SeedPostgresWalArchivingAlertRulesMigration
        .down(&manager)
        .await?;

    let watched = insert_service(&txn, "orders-db", "postgres", true).await?;
    let unmetered = insert_service(&txn, "reports-db", "postgres", false).await?;
    let cache = insert_service(&txn, "cache", "redis", true).await?;
    let tuned = insert_service(&txn, "tuned-db", "postgres", true).await?;
    insert_rule(&txn, tuned, "pg.archiver_failing", 0.0).await?;
    let full = insert_service(&txn, "full-db", "postgres", true).await?;
    for index in 0..99 {
        insert_rule(&txn, full, &format!("custom.metric_{index}"), 1.0).await?;
    }

    SeedPostgresWalArchivingAlertRulesMigration
        .up(&manager)
        .await?;
    // A second run adds nothing.
    SeedPostgresWalArchivingAlertRulesMigration
        .up(&manager)
        .await?;

    assert_eq!(
        archiving_rules(&txn, watched).await?,
        vec![
            ("pg.archiver_failing".to_string(), 0.5, 600),
            ("pg.wal_dir_to_max_wal_size_ratio".to_string(), 4.0, 900),
        ]
    );
    assert!(archiving_rules(&txn, unmetered).await?.is_empty());
    assert!(archiving_rules(&txn, cache).await?.is_empty());
    assert!(
        archiving_rules(&txn, full).await?.is_empty(),
        "a service without room for both rules is left alone"
    );
    assert_eq!(
        archiving_rules(&txn, tuned).await?,
        vec![
            ("pg.archiver_failing".to_string(), 0.0, 60),
            ("pg.wal_dir_to_max_wal_size_ratio".to_string(), 4.0, 900),
        ],
        "the operator's own rule on a metric is kept as it is"
    );

    SeedPostgresWalArchivingAlertRulesMigration
        .down(&manager)
        .await?;
    assert!(archiving_rules(&txn, watched).await?.is_empty());
    assert_eq!(
        archiving_rules(&txn, tuned).await?,
        vec![("pg.archiver_failing".to_string(), 0.0, 60)],
        "down removes only the rules it added"
    );

    txn.rollback().await?;
    Ok(())
}
