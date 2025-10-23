use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

use temps_migrations::Migrator;

/// Test that migrations can be applied successfully
#[tokio::test]
async fn test_migration_up() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (external databases may already have migrations applied)
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!(
            "⏭️  Skipping test_migration_up: using external database via TEMPS_TEST_DATABASE_URL"
        );
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
        .expect("Failed to start TimescaleDB container");

    let port = postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get postgres port");

    // Create database connection string
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    // Wait a bit for the database to be ready, then connect with retries
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(e) if retries > 0 => {
                retries -= 1;
                println!(
                    "Database connection failed, retrying in 2s... ({} retries left)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("Failed to connect to database after retries: {}", e);
                }
            }
            Err(e) => panic!("Failed to connect to database: {}", e),
        }
    };

    // Run migrations
    let result = Migrator::up(&db, None).await;

    match result {
        Ok(_) => {
            println!("✅ Migration up succeeded");

            // Verify that key tables exist
            verify_tables_exist(&db).await?;

            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Migration up failed: {}", e);
            Err(anyhow::Error::from(e))
        }
    }
}

/// Test that migrations can be rolled back successfully
#[tokio::test]
async fn test_migration_down() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (running down migrations would destroy data in external database)
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!(
            "⏭️  Skipping test_migration_down: using external database via TEMPS_TEST_DATABASE_URL"
        );
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
        .expect("Failed to start TimescaleDB container");

    let port = postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get postgres port");

    // Create database connection string
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    // Wait a bit for the database to be ready, then connect with retries
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(e) if retries > 0 => {
                retries -= 1;
                println!(
                    "Database connection failed, retrying in 2s... ({} retries left)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("Failed to connect to database after retries: {}", e);
                }
            }
            Err(e) => panic!("Failed to connect to database: {}", e),
        }
    };

    // First apply migrations
    Migrator::up(&db, None)
        .await
        .expect("Failed to apply migrations");

    // Then roll them back
    let result = Migrator::down(&db, None).await;

    match result {
        Ok(_) => {
            println!("✅ Migration down succeeded");

            // Verify that tables are dropped
            verify_tables_dropped(&db).await?;

            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Migration down failed: {}", e);
            Err(anyhow::Error::from(e))
        }
    }
}

/// Test migration status
#[tokio::test]
async fn test_migration_status() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (external databases may already have migrations applied)
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!("⏭️  Skipping test_migration_status: using external database via TEMPS_TEST_DATABASE_URL");
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
        .expect("Failed to start TimescaleDB container");

    let port = postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get postgres port");

    // Create database connection string
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    // Wait a bit for the database to be ready, then connect with retries
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(e) if retries > 0 => {
                retries -= 1;
                println!(
                    "Database connection failed, retrying in 2s... ({} retries left)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("Failed to connect to database after retries: {}", e);
                }
            }
            Err(e) => panic!("Failed to connect to database: {}", e),
        }
    };

    // Check status before migrations
    let status_before = Migrator::get_pending_migrations(&db).await?;
    assert!(!status_before.is_empty(), "Should have pending migrations");

    // Apply migrations
    Migrator::up(&db, None).await?;

    // Check status after migrations
    let status_after = Migrator::get_pending_migrations(&db).await?;
    assert!(
        status_after.is_empty(),
        "Should have no pending migrations after up"
    );

    // Note: Migrator::fresh doesn't work well with TimescaleDB extensions
    // So we skip the fresh test for now

    println!("✅ Migration status operations succeeded");
    Ok(())
}

/// Test that pgvector extension is properly handled
#[tokio::test]
async fn test_pgvector_extension() -> anyhow::Result<()> {
    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
        .expect("Failed to start TimescaleDB container");

    let port = postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get postgres port");

    // Create database connection string
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    // Wait a bit for the database to be ready, then connect with retries
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(e) if retries > 0 => {
                retries -= 1;
                println!(
                    "Database connection failed, retrying in 2s... ({} retries left)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("Failed to connect to database after retries: {}", e);
                }
            }
            Err(e) => panic!("Failed to connect to database: {}", e),
        }
    };

    // Apply migrations (this should handle pgvector gracefully)
    Migrator::up(&db, None).await?;

    // Check if pgvector extension exists
    let has_vector = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')".to_string(),
        ))
        .await?;

    let has_vector_ext = has_vector
        .and_then(|row| row.try_get::<bool>("", "exists").ok())
        .unwrap_or(false);

    if has_vector_ext {
        println!("✅ pgvector extension is available and properly handled");

        // Verify that error_groups table has vector embedding column
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT data_type FROM information_schema.columns WHERE table_name = 'error_groups' AND column_name = 'embedding'".to_string(),
            ))
            .await;

        if let Ok(Some(row)) = result {
            let data_type: String = row.try_get("", "data_type").unwrap_or_default();
            assert!(
                data_type.contains("USER-DEFINED") || data_type.contains("vector"),
                "Expected vector type for embedding column, got: {}",
                data_type
            );
            println!("✅ Vector embedding column properly created");
        }
    } else {
        println!("ℹ️  pgvector extension not available, fallback to text column handled");

        // Verify that error_groups table has text embedding column
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT data_type FROM information_schema.columns WHERE table_name = 'error_groups' AND column_name = 'embedding'".to_string(),
            ))
            .await;

        if let Ok(Some(row)) = result {
            let data_type: String = row.try_get("", "data_type").unwrap_or_default();
            assert_eq!(
                data_type, "text",
                "Expected text type for embedding column fallback, got: {}",
                data_type
            );
            println!("✅ Text embedding column fallback properly created");
        }
    }

    Ok(())
}

/// Test specific table creation and constraints
#[tokio::test]
async fn test_table_constraints() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (external databases may already have migrations applied)
    if std::env::var("TEMPS_TEST_DATABASE_URL").is_ok() {
        println!("⏭️  Skipping test_table_constraints: using external database via TEMPS_TEST_DATABASE_URL");
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
        .expect("Failed to start TimescaleDB container");

    let port = postgres_container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get postgres port");

    // Create database connection string
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    // Wait a bit for the database to be ready, then connect with retries
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    let mut retries = 5;
    let db = loop {
        match Database::connect(&db_url).await {
            Ok(db) => break db,
            Err(e) if retries > 0 => {
                retries -= 1;
                println!(
                    "Database connection failed, retrying in 2s... ({} retries left)",
                    retries
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    panic!("Failed to connect to database after retries: {}", e);
                }
            }
            Err(e) => panic!("Failed to connect to database: {}", e),
        }
    };

    // Apply migrations
    Migrator::up(&db, None).await?;

    // Test foreign key constraints
    verify_foreign_keys(&db).await?;

    // Test indexes
    verify_indexes(&db).await?;

    // Test unique constraints
    verify_unique_constraints(&db).await?;

    println!("✅ Table constraints verified successfully");
    Ok(())
}

async fn verify_tables_exist(db: &DatabaseConnection) -> anyhow::Result<()> {
    let tables = vec![
        "users",
        "projects",
        "environments",
        "deployments",
        "visitor",
        "ip_geolocations",
        "session_replay_sessions",
        "error_groups",
        "error_events",
        "project_dsns",
        "error_sessions",
        "error_attachments",
        "error_user_feedback",
    ];

    for table in tables {
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = '{}')",
                    table
                ),
            ))
            .await?;

        if let Some(row) = result {
            let exists: bool = row.try_get("", "exists")?;
            assert!(exists, "Table {} should exist after migration up", table);
        }
    }

    println!("✅ All expected tables exist");
    Ok(())
}

async fn verify_tables_dropped(db: &DatabaseConnection) -> anyhow::Result<()> {
    let tables = vec![
        "error_user_feedback",
        "error_attachments",
        "error_sessions",
        "project_dsns",
        "error_events",
        "error_groups",
        "session_replay_sessions",
        "ip_geolocations",
        "visitor",
        "deployments",
        "environments",
        "projects",
        "users",
    ];

    for table in tables {
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = '{}')",
                    table
                ),
            ))
            .await?;

        if let Some(row) = result {
            let exists: bool = row.try_get("", "exists")?;
            assert!(
                !exists,
                "Table {} should not exist after migration down",
                table
            );
        }
    }

    println!("✅ All tables properly dropped");
    Ok(())
}

async fn verify_foreign_keys(db: &DatabaseConnection) -> anyhow::Result<()> {
    // Check some key foreign key constraints exist
    let fk_constraints = vec![
        ("error_events", "fk_error_events_error_group_id"),
        ("error_events", "fk_error_events_project_id"),
        ("error_groups", "fk_error_groups_project_id"),
        ("project_dsns", "fk_project_dsns_project"),
        ("projects", "fk_projects_user_id"),
    ];

    for (table, constraint) in fk_constraints {
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("SELECT EXISTS (SELECT 1 FROM information_schema.table_constraints WHERE constraint_name = '{}' AND table_name = '{}' AND constraint_type = 'FOREIGN KEY')", constraint, table),
            ))
            .await?;

        if let Some(row) = result {
            let exists: bool = row.try_get("", "exists")?;
            assert!(
                exists,
                "Foreign key constraint {} should exist on table {}",
                constraint, table
            );
        }
    }

    println!("✅ Foreign key constraints verified");
    Ok(())
}

async fn verify_indexes(db: &DatabaseConnection) -> anyhow::Result<()> {
    // Check some key indexes exist
    let indexes = vec![
        "idx_error_events_project_id",
        "idx_error_events_timestamp",
        "idx_error_groups_project_id",
        "idx_project_dsns_public_key",
        "idx_error_sessions_project_id",
    ];

    for index in indexes {
        let result = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = '{}')",
                    index
                ),
            ))
            .await?;

        if let Some(row) = result {
            let exists: bool = row.try_get("", "exists")?;
            assert!(exists, "Index {} should exist", index);
        }
    }

    println!("✅ Indexes verified");
    Ok(())
}

async fn verify_unique_constraints(db: &DatabaseConnection) -> anyhow::Result<()> {
    // Check unique constraints on critical fields
    let result = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS (SELECT 1 FROM information_schema.table_constraints WHERE constraint_name LIKE '%project_dsns_public_key%' AND constraint_type = 'UNIQUE')".to_string(),
        ))
        .await?;

    if let Some(row) = result {
        let exists: bool = row.try_get("", "exists")?;
        assert!(
            exists,
            "Unique constraint on project_dsns.public_key should exist"
        );
    }

    println!("✅ Unique constraints verified");
    Ok(())
}
