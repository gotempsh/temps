use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use temps_migrations::Migrator;
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

const TARGET: &str = "m20260805_000001_index_normalized_managed_domains";
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
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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

    // A second full `up` is a no-op and must preserve the index.
    Migrator::up(&db, None).await?;
    assert!(
        index_exists(&db).await?,
        "repeated up must preserve the index"
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

    let after = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT count(*)::int AS n FROM seaql_migrations WHERE version > '{TARGET}'"),
        ))
        .await?
        .expect("migration count query returns one row");
    let steps_after: i32 = after.try_get("", "n")?;
    Migrator::down(&db, Some(steps_after as u32 + 1)).await?;
    assert!(
        !index_exists(&db).await?,
        "down must remove the normalized-domain index"
    );

    Migrator::up(&db, None).await?;
    assert!(
        index_exists(&db).await?,
        "up after rollback must restore the normalized-domain index"
    );

    Ok(())
}
