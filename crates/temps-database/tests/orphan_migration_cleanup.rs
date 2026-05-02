//! Regression test for the prod upgrade failure where Sea-ORM panicked
//! during the migration load phase because `seaql_migrations` contained
//! rows for migrations whose source files had been removed (typically
//! after a squash). Reproduces the prod scenario, then verifies that
//! `cleanup_orphaned_migrations` strips the orphaned rows so the
//! subsequent `Migrator::up()` call succeeds.

use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use temps_database::cleanup_orphaned_migrations;
use temps_migrations::Migrator;
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

#[tokio::test]
async fn cleanup_strips_orphans_so_migrator_can_load() -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!(
            "⏭️  Skipping cleanup_strips_orphans_so_migrator_can_load: external database in use"
        );
        return Ok(());
    }

    // Spin up a fresh TimescaleDB container.
    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await?;
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(_) if retries > 0 => {
                retries -= 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    };

    // ── Bring the schema up to current main first. ───────────────────
    Migrator::up(&db, None).await?;

    // ── Inject the prod failure mode: pretend a previous build applied
    //    a now-removed migration. These names mirror the ones reported
    //    in the prod log (compose stack migrations that were squashed). ─
    let orphans = [
        "m20260321_000001_create_compose_stacks",
        "m20260323_000001_create_compose_stack_routes",
        "m20260323_000002_add_compose_stack_repo_source",
        "m20260323_000003_add_compose_stack_port_overrides",
    ];
    for v in &orphans {
        db.execute(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO seaql_migrations (version, applied_at) \
             VALUES ($1, extract(epoch from now())::bigint) \
             ON CONFLICT DO NOTHING",
            vec![sea_orm::Value::from(v.to_string())],
        ))
        .await?;
    }

    // ── Sea-ORM should now refuse to load — that's the prod symptom. ─
    let pre_check = Migrator::up(&db, None).await;
    assert!(
        pre_check.is_err(),
        "expected Migrator::up to fail when seaql_migrations has orphans, \
         got Ok(()) — test premise broken"
    );

    // ── Cleanup must succeed and remove every orphan. ────────────────
    cleanup_orphaned_migrations(&db).await?;

    let count_row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) AS n FROM seaql_migrations \
             WHERE version LIKE 'm20260321_%' \
                OR version LIKE 'm20260323_00000[123]_%'"
                .to_string(),
        ))
        .await?
        .expect("count row");
    let remaining: i64 = count_row.try_get("", "n")?;
    assert_eq!(
        remaining, 0,
        "all orphan rows must be deleted (got {} remaining)",
        remaining
    );

    // ── After cleanup, Sea-ORM must load and run migrations cleanly. ─
    Migrator::up(&db, None).await?;

    println!("✅ orphaned migration rows cleaned up; Migrator::up succeeded");
    Ok(())
}

#[tokio::test]
async fn cleanup_is_noop_on_clean_db() -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!("⏭️  Skipping cleanup_is_noop_on_clean_db: external database in use");
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await?;
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(_) if retries > 0 => {
                retries -= 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    };

    // No seaql_migrations table yet — must be a no-op, not an error.
    cleanup_orphaned_migrations(&db).await?;

    // Run migrations to create the table, then call cleanup again —
    // still a no-op because every applied row matches a known migration.
    Migrator::up(&db, None).await?;
    cleanup_orphaned_migrations(&db).await?;

    println!("✅ cleanup is a no-op on a clean DB");
    Ok(())
}
