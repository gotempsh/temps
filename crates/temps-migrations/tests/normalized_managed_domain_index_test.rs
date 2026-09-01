// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use temps_migrations::Migrator;
use temps_migrations::NormalizedManagedDomainIndexMigration;
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};

const INDEX_NAME: &str = "idx_dns_managed_domains_normalized_domain";

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

async fn index_exists(db: &DatabaseConnection) -> anyhow::Result<bool> {
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT to_regclass('public.{INDEX_NAME}') IS NOT NULL AS present"),
        ))
        .await?
        .expect("index existence query returns one row");
    Ok(row.try_get("", "present")?)
}

async fn index_definition<C>(db: &C) -> anyhow::Result<String>
where
    C: ConnectionTrait,
{
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT pg_get_indexdef('{INDEX_NAME}'::regclass) AS definition"),
        ))
        .await?
        .expect("index definition query returns one row");
    Ok(row.try_get("", "definition")?)
}

#[tokio::test]
async fn test_normalized_managed_domain_index_migration_is_used_and_reversible(
) -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        eprintln!("Skipping normalized-domain index migration test: external database in use");
        return Ok(());
    }

    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
        // Without this, `start()` returns before PostgreSQL accepts clients
        // and the test races the database ("Connection reset by peer").
        // Must match on stderr: the temporary initdb server logs its "ready"
        // line to stdout, the real server to stderr.
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
            eprintln!(
                "Skipping normalized-domain index migration test: Docker unavailable: {error}"
            );
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let database_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&database_url).await?;

    Migrator::up(&db, None).await?;
    assert!(
        index_exists(&db).await?,
        "normalized-domain index must exist after up"
    );

    // Invoke this migration directly even though the migrator already created
    // the index. Both calls execute the CREATE INDEX IF NOT EXISTS statement.
    let transaction = db.begin().await?;
    let manager = SchemaManager::new(&transaction);
    NormalizedManagedDomainIndexMigration.up(&manager).await?;
    NormalizedManagedDomainIndexMigration.up(&manager).await?;
    assert_eq!(
        transaction
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT current_setting('lock_timeout') AS lock_timeout, \
                        current_setting('statement_timeout') AS statement_timeout"
                    .to_string(),
            ))
            .await?
            .expect("timeout settings query returns one row")
            .try_get::<String>("", "lock_timeout")?,
        "5s"
    );
    let timeout_row = transaction
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT current_setting('statement_timeout') AS statement_timeout".to_string(),
        ))
        .await?
        .expect("statement timeout query returns one row");
    assert_eq!(
        timeout_row.try_get::<String>("", "statement_timeout")?,
        "30s"
    );
    transaction.commit().await?;
    assert!(
        index_exists(&db).await?,
        "direct repeated up must preserve the index"
    );
    let definition = index_definition(&db).await?;
    assert!(
        definition.contains(
            "lower(regexp_replace(rtrim(btrim((domain)::text), '.'::text), '^((\\*\\.)+)'::text, ''::text))"
        ),
        "index must retain the canonical managed-domain expression; definition was: {definition}"
    );

    db.execute_unprepared(
        "INSERT INTO dns_providers (name, provider_type, credentials) \
         VALUES ('index-plan-provider', 'manual', 'unused')",
    )
    .await?;
    db.execute_unprepared(
        "INSERT INTO dns_managed_domains (provider_id, domain, verified) \
         SELECT (SELECT id FROM dns_providers WHERE name = 'index-plan-provider'), \
                'zone-' || value || '.example.test', true \
         FROM generate_series(1, 2000) AS value",
    )
    .await?;
    db.execute_unprepared("SET enable_seqscan = off").await?;
    let plan_rows = db
        .query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "EXPLAIN SELECT * FROM dns_managed_domains \
             WHERE LOWER(REGEXP_REPLACE(RTRIM(BTRIM(\"dns_managed_domains\".\"domain\"), '.'), '^((\\*\\.)+)', '')) \
                   IN ('zone-1500.example.test')"
                .to_string(),
        ))
        .await?;
    let plan = plan_rows
        .iter()
        .map(|row| row.try_get::<String>("", "QUERY PLAN"))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    assert!(
        plan.contains(INDEX_NAME),
        "planner must use {INDEX_NAME}; plan was:\n{plan}"
    );

    let transaction = db.begin().await?;
    let manager = SchemaManager::new(&transaction);
    NormalizedManagedDomainIndexMigration.down(&manager).await?;
    let absent = transaction
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT to_regclass('public.{INDEX_NAME}') IS NULL AS absent"),
        ))
        .await?
        .expect("index absence query returns one row");
    assert!(
        absent.try_get::<bool>("", "absent")?,
        "direct down must remove the normalized-domain index"
    );
    NormalizedManagedDomainIndexMigration.up(&manager).await?;
    assert!(
        transaction
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("SELECT to_regclass('public.{INDEX_NAME}') IS NOT NULL AS present"),
            ))
            .await?
            .expect("restored index query returns one row")
            .try_get::<bool>("", "present")?,
        "direct up after direct down must restore the normalized-domain index"
    );
    transaction.commit().await?;

    Ok(())
}
