//! Schema-level coverage for the ADR-036 workspace lifecycle migration.
//!
//! Three things this pins that a unit test cannot:
//!
//! 1. **Pre-existing rows stay ephemeral.** The whole compatibility story
//!    rests on the column default applying to rows the migration didn't
//!    write. A row inserted without `lifecycle` must read `'ephemeral'`,
//!    not NULL and not an error.
//! 2. **The partial indexes carry the right predicates.** `list_for_user`
//!    filters on `lifecycle`/`project_id` expecting index support; an
//!    index created over the wrong predicate would still exist and still
//!    be silently useless.
//! 3. **`down` is real.** A migration that can't be rolled back strands
//!    an operator who upgrades and needs to go back.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, TransactionTrait};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use temps_migrations::{Migrator, SandboxWorkspaceLifecycleMigration};
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

const LIFECYCLE_INDEX: &str = "idx_sandboxes_lifecycle";
const PROJECT_INDEX: &str = "idx_sandboxes_project_id";

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

async fn relation_exists<C>(db: &C, name: &str) -> anyhow::Result<bool>
where
    C: ConnectionTrait,
{
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT to_regclass('public.{name}') IS NOT NULL AS present"),
        ))
        .await?
        .expect("relation existence query returns one row");
    Ok(row.try_get("", "present")?)
}

async fn index_definition<C>(db: &C, name: &str) -> anyhow::Result<String>
where
    C: ConnectionTrait,
{
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT pg_get_indexdef('{name}'::regclass) AS definition"),
        ))
        .await?
        .expect("index definition query returns one row");
    Ok(row.try_get("", "definition")?)
}

#[tokio::test]
async fn test_sandbox_workspace_lifecycle_migration_defaults_indexes_and_reverses(
) -> anyhow::Result<()> {
    if std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        eprintln!("Skipping sandbox workspace lifecycle migration test: external database in use");
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
                "Skipping sandbox workspace lifecycle migration test: Docker unavailable: {error}"
            );
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let database_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&database_url).await?;

    Migrator::up(&db, None).await?;

    // A row written the way every pre-ADR-036 caller wrote one: no
    // lifecycle, no project, no repo. It must come back ephemeral.
    db.execute_unprepared(
        "INSERT INTO sandboxes \
           (public_id, user_id, name, status, work_dir, timeout_secs, \
            created_at, last_activity_at, expires_at) \
         VALUES \
           ('sbx_legacyrow000001', NULL, 'legacy', 'running', '/workspace', 3600, \
            NOW(), NOW(), NOW() + INTERVAL '1 hour')",
    )
    .await?;
    let legacy = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT lifecycle, project_id IS NULL AS no_project, \
                    source_repo_url IS NULL AS no_repo \
             FROM sandboxes WHERE public_id = 'sbx_legacyrow000001'"
                .to_string(),
        ))
        .await?
        .expect("the inserted legacy row is readable");
    assert_eq!(
        legacy.try_get::<String>("", "lifecycle")?,
        "ephemeral",
        "rows written without a lifecycle must default to ephemeral — \
         anything else silently changes the behaviour of existing sandboxes"
    );
    assert!(legacy.try_get::<bool>("", "no_project")?);
    assert!(legacy.try_get::<bool>("", "no_repo")?);

    // NOT NULL is what lets `SandboxLifecycle::from_row` read a plain
    // String instead of an Option.
    let nullability = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'sandboxes' \
               AND column_name = 'lifecycle'"
                .to_string(),
        ))
        .await?
        .expect("lifecycle column metadata query returns one row");
    assert_eq!(nullability.try_get::<String>("", "is_nullable")?, "NO");
    assert!(nullability
        .try_get::<String>("", "column_default")?
        .contains("ephemeral"));

    assert!(relation_exists(&db, LIFECYCLE_INDEX).await?);
    assert!(relation_exists(&db, PROJECT_INDEX).await?);

    // Predicates matter: an index over every row would defeat the point
    // of keeping the ephemeral majority out of the workspace queries.
    let lifecycle_def = index_definition(&db, LIFECYCLE_INDEX).await?;
    assert!(
        lifecycle_def.contains("WHERE") && lifecycle_def.contains("ephemeral"),
        "lifecycle index must stay partial on non-ephemeral rows; definition was: {lifecycle_def}"
    );
    let project_def = index_definition(&db, PROJECT_INDEX).await?;
    assert!(
        project_def.contains("project_id IS NOT NULL"),
        "project index must stay partial on attributed rows; definition was: {project_def}"
    );

    // A workspace row round-trips, including the two new nullable columns.
    db.execute_unprepared(
        "INSERT INTO sandboxes \
           (public_id, user_id, name, status, work_dir, timeout_secs, \
            created_at, last_activity_at, expires_at, lifecycle, project_id, source_repo_url) \
         VALUES \
           ('sbx_workspacerow0001', NULL, 'ws', 'stopped', '/workspace', 3600, \
            NOW(), NOW(), NOW(), 'workspace', 42, 'https://github.com/org/repo.git')",
    )
    .await?;
    let workspace = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT lifecycle, project_id, source_repo_url \
             FROM sandboxes WHERE public_id = 'sbx_workspacerow0001'"
                .to_string(),
        ))
        .await?
        .expect("the inserted workspace row is readable");
    assert_eq!(workspace.try_get::<String>("", "lifecycle")?, "workspace");
    assert_eq!(workspace.try_get::<i32>("", "project_id")?, 42);
    assert_eq!(
        workspace.try_get::<String>("", "source_repo_url")?,
        "https://github.com/org/repo.git"
    );

    // Rolled back in a transaction so the assertions below don't depend on
    // dropping columns that still hold the rows inserted above.
    let transaction = db.begin().await?;
    let manager = SchemaManager::new(&transaction);
    SandboxWorkspaceLifecycleMigration.down(&manager).await?;
    assert!(
        !relation_exists(&transaction, LIFECYCLE_INDEX).await?,
        "down must drop the lifecycle index"
    );
    assert!(
        !relation_exists(&transaction, PROJECT_INDEX).await?,
        "down must drop the project index"
    );
    let remaining = transaction
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT COUNT(*) AS n FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'sandboxes' \
               AND column_name IN ('lifecycle', 'project_id', 'source_repo_url')"
                .to_string(),
        ))
        .await?
        .expect("column count query returns one row");
    assert_eq!(
        remaining.try_get::<i64>("", "n")?,
        0,
        "down must drop all three columns"
    );

    // Re-applying inside the same transaction proves up is replayable
    // after a rollback, which is what an operator retrying an upgrade does.
    SandboxWorkspaceLifecycleMigration.up(&manager).await?;
    SandboxWorkspaceLifecycleMigration.up(&manager).await?;
    assert!(relation_exists(&transaction, LIFECYCLE_INDEX).await?);
    assert!(relation_exists(&transaction, PROJECT_INDEX).await?);
    transaction.rollback().await?;

    Ok(())
}
