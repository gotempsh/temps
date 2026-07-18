use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

use temps_migrations::Migrator;

/// True when an external database is configured. CI can only *empty* an env
/// var per matrix entry, not unset it, so empty counts as "not configured" —
/// otherwise the skip-guards below would fire in the dedicated migrations
/// lane and this suite would (again) never actually run anywhere.
fn external_db_configured() -> bool {
    std::env::var("TEMPS_TEST_DATABASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// Test that migrations can be applied successfully
#[tokio::test]
async fn test_migration_up() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (external databases may already have migrations applied)
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_migration_up: using external database via TEMPS_TEST_DATABASE_URL"
        );
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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

#[tokio::test]
async fn test_secure_sns_migration_upgrades_applied_global_suppression_schema() -> anyhow::Result<()>
{
    if external_db_configured() {
        return Ok(());
    }

    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
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
            eprintln!("Skipping secure SNS migration test: Docker unavailable: {error}");
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    let target = "m20260714_000001_secure_sns_email_events";
    let pre_target_count = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == target)
        .expect("secure SNS migration must be registered");
    Migrator::up(&db, Some(pre_target_count as u32)).await?;

    db.execute_unprepared(
        r#"
        INSERT INTO email_providers (name, provider_type, region, credentials)
            VALUES ('legacy-migration-test', 'ses', 'us-east-1', 'test');
        INSERT INTO email_domains (provider_id, domain)
            SELECT id, domain
            FROM email_providers
            CROSS JOIN (VALUES
                ('legacy-one.example'), ('legacy-two.example')
            ) AS domains(domain)
            WHERE name = 'legacy-migration-test';
        "#,
    )
    .await?;

    // Reproduce the exact schema #296 installed before this PR changed it.
    db.execute_unprepared(
        r#"
        DROP INDEX IF EXISTS idx_suppressed_recipients_domain_email;
        ALTER TABLE suppressed_recipients ALTER COLUMN domain_id DROP NOT NULL;
        ALTER TABLE suppressed_recipients
            DROP CONSTRAINT IF EXISTS suppressed_recipients_domain_id_fkey;
        ALTER TABLE suppressed_recipients
            ADD CONSTRAINT suppressed_recipients_domain_id_fkey
            FOREIGN KEY (domain_id) REFERENCES email_domains(id) ON DELETE SET NULL;
        CREATE UNIQUE INDEX idx_suppressed_recipients_email
            ON suppressed_recipients (email);
        INSERT INTO suppressed_recipients (email, reason, domain_id)
            VALUES ('legacy-unscoped@example.com', 'bounced', NULL);
        "#,
    )
    .await?;

    Migrator::up(&db, None).await?;

    let nullable = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT is_nullable FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'suppressed_recipients' \
               AND column_name = 'domain_id'"
                .to_string(),
        ))
        .await?
        .expect("domain_id schema row");
    let is_nullable: String = nullable.try_get("", "is_nullable")?;
    assert_eq!(is_nullable, "NO");

    let legacy_count = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS count FROM suppressed_recipients \
             WHERE domain_id IS NULL"
                .to_string(),
        ))
        .await?
        .expect("legacy suppression count");
    let count: i32 = legacy_count.try_get("", "count")?;
    assert_eq!(count, 0, "unscoped suppressions must gain domain ownership");

    let expanded_count = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS count FROM suppressed_recipients \
             WHERE email = 'legacy-unscoped@example.com'"
                .to_string(),
        ))
        .await?
        .expect("expanded legacy suppression count");
    let count: i32 = expanded_count.try_get("", "count")?;
    assert_eq!(
        count, 2,
        "legacy global suppression must cover every existing domain"
    );

    let index = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT indexdef FROM pg_indexes \
             WHERE schemaname = current_schema() \
               AND indexname = 'idx_suppressed_recipients_domain_email'"
                .to_string(),
        ))
        .await?
        .expect("domain-scoped unique index");
    let indexdef: String = index.try_get("", "indexdef")?;
    assert!(indexdef.contains("UNIQUE"));
    assert!(indexdef.contains("domain_id, email"));

    // The legacy global unique index must be gone: the same recipient can be
    // suppressed independently for two sending domains.
    db.execute_unprepared(
        r#"
        INSERT INTO email_providers (name, provider_type, region, credentials)
            VALUES ('migration-test', 'ses', 'us-east-1', 'test');
        INSERT INTO email_domains (provider_id, domain)
            SELECT id, domain
            FROM email_providers
            CROSS JOIN (VALUES ('one.example'), ('two.example')) AS domains(domain)
            WHERE name = 'migration-test';
        INSERT INTO suppressed_recipients (email, reason, domain_id)
            SELECT 'shared@example.com', 'bounced', id
            FROM email_domains
            WHERE domain IN ('one.example', 'two.example');

        WITH inserted_email AS (
            INSERT INTO emails (
                domain_id, from_address, to_addresses, subject,
                provider_message_id
            )
            SELECT id, 'sender@one.example', '["shared@example.com"]'::jsonb,
                   'migration rollback test', 'ses-message-id'
            FROM email_domains
            WHERE domain = 'one.example'
            RETURNING id
        )
        INSERT INTO email_events (
            email_id, event_type, provider_message_id, recipient,
            idempotency_key
        )
        SELECT id, 'bounced', 'ses-message-id', recipient, idempotency_key
        FROM inserted_email
        CROSS JOIN (VALUES
            ('first@example.com', repeat('a', 64)),
            ('second@example.com', repeat('b', 64))
        ) AS events(recipient, idempotency_key);
        "#,
    )
    .await?;

    let scoped_count = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS count FROM suppressed_recipients \
             WHERE email = 'shared@example.com'"
                .to_string(),
        ))
        .await?
        .expect("domain-scoped suppression count");
    let count: i32 = scoped_count.try_get("", "count")?;
    assert_eq!(count, 2);

    // Roll back exactly through the secure-sns migration, wherever it sits
    // in the chain. A hardcoded step count breaks every time a newer
    // migration lands after it (versions sort lexicographically ==
    // chronologically under the mYYYYMMDD naming scheme).
    let after = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS n FROM seaql_migrations \
             WHERE version > 'm20260714_000001_secure_sns_email_events'"
                .to_string(),
        ))
        .await?
        .expect("seaql_migrations count");
    let steps_after: i32 = after.try_get("", "n")?;
    Migrator::down(&db, Some(steps_after as u32 + 1)).await?;

    let rollback_counts = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT \
                (SELECT count(*)::int FROM suppressed_recipients \
                 WHERE email = 'shared@example.com') AS suppressions, \
                (SELECT count(*)::int FROM email_events \
                 WHERE provider_message_id = 'ses-message-id') AS correlated_events, \
                (SELECT count(*)::int FROM email_events \
                 WHERE provider_message_id IS NULL) AS uncorrelated_events"
                .to_string(),
        ))
        .await?
        .expect("rollback compatibility counts");
    let suppressions: i32 = rollback_counts.try_get("", "suppressions")?;
    let correlated_events: i32 = rollback_counts.try_get("", "correlated_events")?;
    let uncorrelated_events: i32 = rollback_counts.try_get("", "uncorrelated_events")?;
    assert_eq!(
        suppressions, 1,
        "legacy global suppression must be restored"
    );
    assert_eq!(correlated_events, 1, "legacy correlation must stay unique");
    assert_eq!(
        uncorrelated_events, 1,
        "duplicate event rows must be retained"
    );

    Ok(())
}

/// Test that migrations can be rolled back successfully
#[tokio::test]
async fn test_migration_down() -> anyhow::Result<()> {
    // Skip this test if TEMPS_TEST_DATABASE_URL is set
    // (running down migrations would destroy data in external database)
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_migration_down: using external database via TEMPS_TEST_DATABASE_URL"
        );
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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
    if external_db_configured() {
        println!("⏭️  Skipping test_migration_status: using external database via TEMPS_TEST_DATABASE_URL");
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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
    let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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
    if external_db_configured() {
        println!("⏭️  Skipping test_table_constraints: using external database via TEMPS_TEST_DATABASE_URL");
        return Ok(());
    }

    // Start TimescaleDB container
    let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
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
        // m20260427_000001_add_compute_network
        "network_config",
        // m20260427_000002_add_dns_service_endpoints
        "service_endpoints",
        "node_dns_state",
        "dns_generation",
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
    // error_events is a hypertable: the migration replaces its simple
    // single-column indexes with composite time-series indexes, so those
    // are the ones that must exist post-migration.
    let indexes = vec![
        "idx_error_events_project_timestamp",
        "idx_error_events_group_timestamp",
        "idx_error_groups_project_id",
        "idx_project_dsns_public_key",
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

// ---------------------------------------------------------------------------
// Compute-network migration (m20260427_000001) coverage.
//
// We verify the migration end-to-end: columns exist on `nodes`, the
// singleton `network_config` table is created with the default row, the
// CHECK constraints behave correctly (transport must be vxlan/native, id
// must equal 1), and the partial-unique index on nodes.compute_cidr lets
// multiple NULLs coexist while rejecting duplicate non-NULL values.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_compute_network_migration() -> anyhow::Result<()> {
    if external_db_configured() {
        println!("⏭️  Skipping test_compute_network_migration: external database in use");
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;
    Migrator::up(&db, None).await?;

    // ----- nodes.compute_cidr + underlay_address columns exist -----
    for col in ["compute_cidr", "underlay_address"] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                     WHERE table_name = 'nodes' AND column_name = '{}')",
                    col
                ),
            ))
            .await?
            .expect("query returns one row");
        let exists: bool = row.try_get("", "exists")?;
        assert!(exists, "nodes.{} must exist after migration", col);
    }

    // ----- partial-unique index lets multiple NULLs coexist, but not duplicates -----
    db.execute_unprepared(
        "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
         labels, capacity, compute_cidr) \
         VALUES \
            ('a', 'h1', '127.0.0.1', '10.0.0.1', 'worker', 'pending', '{}', '{}', NULL), \
            ('b', 'h2', '127.0.0.2', '10.0.0.2', 'worker', 'pending', '{}', '{}', NULL), \
            ('c', 'h3', '127.0.0.3', '10.0.0.3', 'worker', 'pending', '{}', '{}', '172.20.5.0/24')",
    )
    .await?;
    let dup = db
        .execute_unprepared(
            "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
             labels, capacity, compute_cidr) VALUES \
             ('d', 'h4', '127.0.0.4', '10.0.0.4', 'worker', 'pending', '{}', '{}', '172.20.5.0/24')",
        )
        .await;
    assert!(
        dup.is_err(),
        "duplicate compute_cidr must be rejected, got {:?}",
        dup
    );

    // ----- network_config singleton row exists with defaults -----
    let row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id, compute_pool_cidr, subnet_prefix_len, transport, vxlan_vni, \
             vxlan_port, underlay_mtu FROM network_config"
                .to_string(),
        ))
        .await?
        .expect("network_config singleton row must be present");
    let id: i32 = row.try_get("", "id")?;
    let pool: String = row.try_get("", "compute_pool_cidr")?;
    let prefix: i32 = row.try_get("", "subnet_prefix_len")?;
    let transport: String = row.try_get("", "transport")?;
    let vni: i32 = row.try_get("", "vxlan_vni")?;
    let port: i32 = row.try_get("", "vxlan_port")?;
    let mtu: i32 = row.try_get("", "underlay_mtu")?;
    assert_eq!(id, 1);
    assert_eq!(pool, "172.20.0.0/16");
    assert_eq!(prefix, 24);
    assert_eq!(transport, "vxlan");
    assert_eq!(vni, 42);
    assert_eq!(port, 4789);
    assert_eq!(mtu, 1500);

    // ----- CHECK (id = 1) prevents inserting a second row -----
    let second = db
        .execute_unprepared(
            "INSERT INTO network_config (id, compute_pool_cidr, subnet_prefix_len, \
             transport, vxlan_vni, vxlan_port, underlay_mtu) \
             VALUES (2, '10.0.0.0/16', 24, 'vxlan', 42, 4789, 1500)",
        )
        .await;
    assert!(
        second.is_err(),
        "network_config must be a singleton (id = 1), got {:?}",
        second
    );

    // ----- CHECK on transport rejects unknown values -----
    let bad_transport = db
        .execute_unprepared("UPDATE network_config SET transport = 'gre' WHERE id = 1")
        .await;
    assert!(
        bad_transport.is_err(),
        "transport must be one of (vxlan, native), got {:?}",
        bad_transport
    );

    // ----- valid transport update succeeds -----
    db.execute_unprepared("UPDATE network_config SET transport = 'native' WHERE id = 1")
        .await?;

    println!("✅ compute network migration verified");
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal-DNS migration (m20260427_000002) coverage. ADR-011.
//
// Verifies the migration end-to-end:
//   - service_endpoints + node_dns_state tables exist with the columns we rely on
//   - record_type / owner_kind CHECK constraints reject invalid values
//   - the (fqdn, record_type, target_ip) unique index rejects duplicates but
//     allows multi-A records (same fqdn+type, different IPs)
//   - node_dns_state.health CHECK constraint behaves
//   - FK on node_dns_state.node_id cascades on node delete
//   - FK on service_endpoints.node_id sets to NULL on node delete (records
//     for a removed node remain authoritative until the GC reconciles them)
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_dns_service_endpoints_migration() -> anyhow::Result<()> {
    if external_db_configured() {
        println!("⏭️  Skipping test_dns_service_endpoints_migration: external database in use");
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;
    Migrator::up(&db, None).await?;

    // ----- service_endpoints columns exist -----
    for col in [
        "id",
        "fqdn",
        "record_type",
        "target_ip",
        "target_port",
        "ttl",
        "owner_kind",
        "owner_id",
        "node_id",
        "generation",
        "created_at",
        "updated_at",
    ] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                     WHERE table_name = 'service_endpoints' AND column_name = '{}')",
                    col
                ),
            ))
            .await?
            .expect("query returns one row");
        let exists: bool = row.try_get("", "exists")?;
        assert!(exists, "service_endpoints.{} must exist", col);
    }

    // ----- node_dns_state columns exist -----
    for col in ["node_id", "applied_generation", "last_sync_at", "health"] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                     WHERE table_name = 'node_dns_state' AND column_name = '{}')",
                    col
                ),
            ))
            .await?
            .expect("query returns one row");
        let exists: bool = row.try_get("", "exists")?;
        assert!(exists, "node_dns_state.{} must exist", col);
    }

    // ----- valid A record inserts cleanly -----
    db.execute_unprepared(
        "INSERT INTO service_endpoints \
         (fqdn, record_type, target_ip, target_port, ttl, owner_kind, owner_id, generation) \
         VALUES \
            ('pg-orders-0.pg-orders.temps.local', 'A', '172.20.5.10', 5432, 5, 'service_member', 1, 1)",
    )
    .await?;

    // ----- multi-A allowed: same fqdn+type, different IP -----
    db.execute_unprepared(
        "INSERT INTO service_endpoints \
         (fqdn, record_type, target_ip, target_port, ttl, owner_kind, owner_id, generation) \
         VALUES \
            ('pg-orders.temps.local', 'A', '172.20.5.10', 5432, 5, 'service_role', 1, 2), \
            ('pg-orders.temps.local', 'A', '172.20.6.11', 5432, 5, 'service_role', 1, 2)",
    )
    .await?;

    // ----- duplicate (fqdn, record_type, target_ip) is rejected -----
    let dup = db
        .execute_unprepared(
            "INSERT INTO service_endpoints \
             (fqdn, record_type, target_ip, target_port, ttl, owner_kind, owner_id, generation) \
             VALUES \
                ('pg-orders.temps.local', 'A', '172.20.5.10', 5432, 5, 'service_role', 1, 3)",
        )
        .await;
    assert!(
        dup.is_err(),
        "duplicate (fqdn, record_type, target_ip) must be rejected, got {:?}",
        dup
    );

    // ----- AAAA accepted for v6 (cheap IPv6 readiness, ADR-011 §scope) -----
    db.execute_unprepared(
        "INSERT INTO service_endpoints \
         (fqdn, record_type, target_ip, target_port, ttl, owner_kind, owner_id, generation) \
         VALUES \
            ('pg-orders.temps.local', 'AAAA', 'fd00::5:10', 5432, 5, 'service_role', 1, 4)",
    )
    .await?;

    // ----- record_type CHECK rejects unknown values -----
    let bad_type = db
        .execute_unprepared(
            "INSERT INTO service_endpoints \
             (fqdn, record_type, target_ip, ttl, owner_kind, owner_id, generation) \
             VALUES ('x.temps.local', 'TXT', '1.2.3.4', 30, 'static', 1, 5)",
        )
        .await;
    assert!(
        bad_type.is_err(),
        "record_type must be one of (A, AAAA, SRV, CNAME), got {:?}",
        bad_type
    );

    // ----- owner_kind CHECK rejects unknown values -----
    let bad_owner = db
        .execute_unprepared(
            "INSERT INTO service_endpoints \
             (fqdn, record_type, target_ip, ttl, owner_kind, owner_id, generation) \
             VALUES ('y.temps.local', 'A', '1.2.3.4', 30, 'whatever', 1, 6)",
        )
        .await;
    assert!(
        bad_owner.is_err(),
        "owner_kind must be one of \
         (service_member, service_role, node, static), got {:?}",
        bad_owner
    );

    // ----- node_dns_state.health CHECK rejects unknown values -----
    // First insert a node we can reference.
    db.execute_unprepared(
        "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
         labels, capacity) \
         VALUES ('worker-1', 'h1', '127.0.0.1', '10.0.0.1', 'worker', 'active', '{}', '{}')",
    )
    .await?;
    let node_id_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id FROM nodes WHERE name = 'worker-1'".to_string(),
        ))
        .await?
        .expect("node row");
    let node_id: i32 = node_id_row.try_get("", "id")?;

    db.execute_unprepared(&format!(
        "INSERT INTO node_dns_state (node_id, applied_generation, health) \
         VALUES ({node_id}, 0, 'healthy')"
    ))
    .await?;

    let bad_health = db
        .execute_unprepared(&format!(
            "UPDATE node_dns_state SET health = 'on-fire' WHERE node_id = {node_id}"
        ))
        .await;
    assert!(
        bad_health.is_err(),
        "health must be one of (healthy, degraded, stale, unknown), got {:?}",
        bad_health
    );

    // ----- valid health update succeeds -----
    db.execute_unprepared(&format!(
        "UPDATE node_dns_state SET health = 'degraded' WHERE node_id = {node_id}"
    ))
    .await?;

    // ----- FK cascade: deleting the node deletes node_dns_state row -----
    db.execute_unprepared(&format!("DELETE FROM nodes WHERE id = {node_id}"))
        .await?;
    let remaining = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT COUNT(*)::int AS c FROM node_dns_state WHERE node_id = {node_id}"),
        ))
        .await?
        .expect("count row");
    let count: i32 = remaining.try_get("", "c")?;
    assert_eq!(
        count, 0,
        "node_dns_state row should cascade-delete with its node"
    );

    // ----- dns_generation singleton seeded with current=0 -----
    let g_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id, current FROM dns_generation".to_string(),
        ))
        .await?
        .expect("dns_generation singleton row");
    let g_id: i32 = g_row.try_get("", "id")?;
    let g_current: i64 = g_row.try_get("", "current")?;
    assert_eq!(g_id, 1);
    assert_eq!(g_current, 0);

    // ----- dns_generation singleton CHECK rejects id != 1 -----
    let bad_id = db
        .execute_unprepared("INSERT INTO dns_generation (id, current) VALUES (2, 0)")
        .await;
    assert!(
        bad_id.is_err(),
        "dns_generation must be a singleton (id = 1), got {:?}",
        bad_id
    );

    // ----- m20260427_000003: service_members.compute_ip column exists -----
    let compute_ip_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_name = 'service_members' AND column_name = 'compute_ip') AS exists"
                .to_string(),
        ))
        .await?
        .expect("query returns one row");
    let exists: bool = compute_ip_row.try_get("", "exists")?;
    assert!(exists, "service_members.compute_ip must exist");

    println!("✅ dns service-endpoints migration verified");
    Ok(())
}

// ---------------------------------------------------------------------------
// Regression test: m20260705_000001_add_visitor_unique_index must repoint ALL
// visitor FK tables before deleting duplicates.
//
// The original migration only handled proxy_logs and request_sessions.  The
// six missing tables were:
//   - session_replay_sessions (CASCADE delete — the critical one: the old code
//     would silently CASCADE-DELETE recording rows instead of repointing them)
//   - performance_metrics, request_logs, events, error_groups, error_events
//     (SetNull — the old code would null out visitor associations that still
//     had a live canonical row to point to)
//
// This test reproduces the CASCADE data-loss scenario by:
//   1. Running every migration up to but not including the target.
//   2. Inserting two visitor rows with the same (visitor_id, project_id).
//      The row with the HIGHER serial id is the "duplicate"; the one with
//      the LOWER id is the "canonical".
//   3. Inserting a session_replay_sessions row whose visitor_id points at
//      the DUPLICATE row (so the old code would have cascade-deleted it).
//   4. Running the target migration.
//   5. Asserting the session_replay_sessions row still exists and now
//      points at the CANONICAL visitor id.
//   6. Asserting the duplicate visitor row is gone and the unique index
//      was created.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_visitor_dedup_migration_repoints_session_replay_sessions() -> anyhow::Result<()> {
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_visitor_dedup_migration_repoints_session_replay_sessions: \
             external database in use"
        );
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    // ── 1. Apply every migration up to but not including the target. ──────
    let target = "m20260705_000001_add_visitor_unique_index";
    let pre_target_count = Migrator::migrations()
        .iter()
        .position(|m| m.name() == target)
        .unwrap_or_else(|| panic!("migration {} not found in Migrator", target));
    Migrator::up(&db, Some(pre_target_count as u32)).await?;

    // Sanity: the target should not be applied yet.
    let pre_state = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT EXISTS (SELECT 1 FROM seaql_migrations WHERE version = '{}') AS applied",
                target
            ),
        ))
        .await?
        .expect("query returns one row");
    let applied: bool = pre_state.try_get("", "applied")?;
    assert!(
        !applied,
        "target migration {} must not be applied before the test body runs",
        target
    );

    // ── 2. Insert minimal prerequisite rows. ─────────────────────────────
    // project
    db.execute_unprepared(
        "INSERT INTO projects (name, repo_name, repo_owner, directory, main_branch, preset, \
         created_at, updated_at, slug) \
         VALUES ('test-proj', 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), 'test-proj-slug')",
    )
    .await?;
    let proj_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT id FROM projects WHERE slug = 'test-proj-slug'".to_string(),
        ))
        .await?
        .expect("project row");
    let proj_id: i32 = proj_row.try_get("", "id")?;

    // environment
    db.execute_unprepared(&format!(
        "INSERT INTO environments (name, slug, subdomain, host, upstreams, \
         created_at, updated_at, project_id) \
         VALUES ('production', 'prod', 'prod', 'prod.example.test', '[]', \
                 now(), now(), {proj_id})"
    ))
    .await?;
    let env_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT id FROM environments WHERE project_id = {proj_id}"),
        ))
        .await?
        .expect("environment row");
    let env_id: i32 = env_row.try_get("", "id")?;

    // deployment
    db.execute_unprepared(&format!(
        "INSERT INTO deployments (project_id, environment_id, created_at, updated_at, \
         slug, state, metadata) \
         VALUES ({proj_id}, {env_id}, now(), now(), 'deploy-1', 'ready', '{{}}'::json)"
    ))
    .await?;
    let dep_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT id FROM deployments WHERE project_id = {proj_id}"),
        ))
        .await?
        .expect("deployment row");
    let dep_id: i32 = dep_row.try_get("", "id")?;

    // ── 3. Insert two visitor rows with the same (visitor_id, project_id). ─
    // The first INSERT gets the lower serial id → that is the canonical row.
    // The second INSERT gets the higher serial id → that is the duplicate.
    let shared_visitor_uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    db.execute_unprepared(&format!(
        "INSERT INTO visitor (visitor_id, first_seen, last_seen, project_id, environment_id) \
         VALUES ('{shared_visitor_uuid}', now(), now(), {proj_id}, {env_id})"
    ))
    .await?;
    let canonical_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT id FROM visitor WHERE visitor_id = '{shared_visitor_uuid}' \
                 ORDER BY id ASC LIMIT 1"
            ),
        ))
        .await?
        .expect("canonical visitor row");
    let canonical_id: i32 = canonical_row.try_get("", "id")?;

    db.execute_unprepared(&format!(
        "INSERT INTO visitor (visitor_id, first_seen, last_seen, project_id, environment_id) \
         VALUES ('{shared_visitor_uuid}', now(), now(), {proj_id}, {env_id})"
    ))
    .await?;
    let duplicate_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT id FROM visitor WHERE visitor_id = '{shared_visitor_uuid}' \
                 ORDER BY id DESC LIMIT 1"
            ),
        ))
        .await?
        .expect("duplicate visitor row");
    let duplicate_id: i32 = duplicate_row.try_get("", "id")?;

    assert!(
        canonical_id < duplicate_id,
        "test setup error: canonical id ({}) must be < duplicate id ({})",
        canonical_id,
        duplicate_id
    );

    // ── 4. Insert a session_replay_sessions row pointing at the DUPLICATE. ─
    // With the unfixed migration this row would be CASCADE-DELETED when the
    // duplicate visitor row is deleted.
    db.execute_unprepared(&format!(
        "INSERT INTO session_replay_sessions \
         (session_replay_id, visitor_id, project_id, environment_id, deployment_id) \
         VALUES ('replay-001', {duplicate_id}, {proj_id}, {env_id}, {dep_id})"
    ))
    .await?;

    // ── 5. Apply the target migration. ───────────────────────────────────
    Migrator::up(&db, None).await?;

    // ── 6. The session_replay_sessions row must STILL EXIST. ─────────────
    let srs_count_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS c FROM session_replay_sessions \
             WHERE session_replay_id = 'replay-001'"
                .to_string(),
        ))
        .await?
        .expect("count row");
    let srs_count: i32 = srs_count_row.try_get("", "c")?;
    assert_eq!(
        srs_count, 1,
        "session_replay_sessions row must survive the migration — \
         CASCADE-DELETE via duplicate visitor deletion is the bug being fixed"
    );

    // ── 7. Its visitor_id must now point at the CANONICAL row. ───────────
    let srs_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT visitor_id FROM session_replay_sessions \
             WHERE session_replay_id = 'replay-001'"
                .to_string(),
        ))
        .await?
        .expect("session_replay_sessions row");
    let repointed_visitor_id: i32 = srs_row.try_get("", "visitor_id")?;
    assert_eq!(
        repointed_visitor_id, canonical_id,
        "session_replay_sessions.visitor_id must be repointed from \
         duplicate ({}) to canonical ({}), got {}",
        duplicate_id, canonical_id, repointed_visitor_id
    );

    // ── 8. The duplicate visitor row must be gone; canonical must remain. ─
    let visitor_count_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT count(*)::int AS c FROM visitor \
                 WHERE visitor_id = '{shared_visitor_uuid}'"
            ),
        ))
        .await?
        .expect("visitor count row");
    let visitor_count: i32 = visitor_count_row.try_get("", "c")?;
    assert_eq!(
        visitor_count, 1,
        "exactly one visitor row must remain after deduplication (got {})",
        visitor_count
    );

    let remaining_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT id FROM visitor WHERE visitor_id = '{shared_visitor_uuid}'"),
        ))
        .await?
        .expect("remaining visitor row");
    let remaining_id: i32 = remaining_row.try_get("", "id")?;
    assert_eq!(
        remaining_id, canonical_id,
        "the remaining visitor row must be the canonical (lowest-id) one"
    );

    // ── 9. The unique index must exist. ───────────────────────────────────
    let idx_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT EXISTS (SELECT 1 FROM pg_indexes \
             WHERE indexname = 'visitor_visitor_id_project_id_key') AS present"
                .to_string(),
        ))
        .await?
        .expect("index query");
    let idx_present: bool = idx_row.try_get("", "present")?;
    assert!(
        idx_present,
        "visitor_visitor_id_project_id_key unique index must exist after migration"
    );

    println!(
        "✅ visitor dedup migration correctly repointed session_replay_sessions \
         from duplicate visitor {} to canonical visitor {} and created unique index",
        duplicate_id, canonical_id
    );
    Ok(())
}

async fn connect_with_retries(db_url: &str) -> anyhow::Result<DatabaseConnection> {
    let mut retries = 5;
    loop {
        match Database::connect(db_url).await {
            Ok(db) => return Ok(db),
            Err(e) if retries > 0 => {
                retries -= 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                if retries == 0 {
                    return Err(anyhow::Error::from(e));
                }
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Regression test: m20260502_000001_add_observe_correlation must succeed even
// when proxy_logs has compressed chunks.
//
// The v1 of that migration ran plain `ALTER TABLE … ADD COLUMN` against
// `proxy_logs`, which on prod-style installs is a TimescaleDB hypertable with
// a 7-day compression policy from `m20260225_000001_add_proxy_logs_retention`.
// Once a chunk compresses, the ALTER fails with `chunk not found` and leaves
// the schema half-applied — observed in production with no clean way forward
// short of manual SQL recovery.
//
// Local dev never caught it because local DBs typically have no rows older
// than 7 days, so no chunk has compressed yet. This test reproduces the
// failure mode by forcing compression on a backfilled chunk before running
// the migrator.
//
// The current migration removes the policy, decompresses every chunk, runs
// the ALTERs, then restores the policy. This test pins that contract.
#[tokio::test]
async fn test_observe_correlation_migration_handles_compressed_proxy_logs() -> anyhow::Result<()> {
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_observe_correlation_migration_handles_compressed_proxy_logs: \
             external database in use"
        );
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    // ── 1. Apply every migration EXCEPT our target so we land on the same
    //       schema state prod was in just before the v1 migration ran. ──────
    let target = "m20260502_000001_add_observe_correlation";
    let pre_target_count = Migrator::migrations()
        .iter()
        .position(|m| m.name() == target)
        .unwrap_or_else(|| panic!("migration {} not found in Migrator", target));
    Migrator::up(&db, Some(pre_target_count as u32)).await?;

    // Sanity: target migration should NOT be applied yet.
    let pre_state = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT EXISTS (SELECT 1 FROM seaql_migrations WHERE version = '{}') AS applied",
                target
            ),
        ))
        .await?
        .expect("query returns one row");
    let applied: bool = pre_state.try_get("", "applied")?;
    assert!(
        !applied,
        "target migration {} should not be applied yet — adjust pre-target count",
        target
    );

    // ── 2. Backfill proxy_logs with rows older than the 7-day compression
    //       window so the resulting chunks are eligible for compression.
    //       The schema needs to match what `m20250101_000001_initial_schema`
    //       defined; we only set the columns required by the NOT NULL
    //       constraints and let the rest default. ──────────────────────────
    db.execute_unprepared(
        "INSERT INTO proxy_logs ( \
             timestamp, method, path, host, status_code, request_source, \
             is_system_request, routing_status, request_id, created_date \
         ) \
         SELECT \
             now() - INTERVAL '10 days' - (s * INTERVAL '1 minute'), \
             'GET', '/api/old/' || s, 'example.test', 200, 'proxy', \
             false, 'routed', 'req-' || s, \
             (now() - INTERVAL '10 days' - (s * INTERVAL '1 minute'))::date \
         FROM generate_series(1, 30) s",
    )
    .await?;

    // ── 3. Force compression on every chunk that's eligible. This mimics
    //       what TimescaleDB's background compression worker does on prod
    //       when chunks age past the 7-day window. ──────────────────────────
    db.execute_unprepared(
        "SELECT compress_chunk(c, if_not_compressed => TRUE) \
         FROM show_chunks('proxy_logs', older_than => now() - INTERVAL '7 days') c",
    )
    .await?;

    let pre = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) FILTER (WHERE is_compressed) AS compressed, count(*) AS total \
             FROM timescaledb_information.chunks WHERE hypertable_name = 'proxy_logs'"
                .to_string(),
        ))
        .await?
        .expect("chunk count");
    let compressed_before: i64 = pre.try_get("", "compressed")?;
    let total_before: i64 = pre.try_get("", "total")?;
    assert!(
        compressed_before > 0,
        "test setup must produce at least one compressed chunk \
         (got {} compressed / {} total) — \
         without that the regression isn't reproducible",
        compressed_before,
        total_before
    );

    // ── 4. Apply the target migration. With v1 this would have errored on
    //       the second ALTER; with the current code it must succeed. ───────
    Migrator::up(&db, None).await?;

    // ── 5. Verify the schema landed correctly. ─────────────────────────────
    for (table, col) in [
        ("proxy_logs", "trace_id"),
        ("proxy_logs", "error_group_id"),
        ("revenue_events", "deployment_id"),
        ("revenue_events", "environment_id"),
        ("revenue_events", "trace_id"),
        ("error_events", "trace_id_indexed"),
    ] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                     WHERE table_name = '{}' AND column_name = '{}') AS present",
                    table, col
                ),
            ))
            .await?
            .expect("col query returns one row");
        let present: bool = row.try_get("", "present")?;
        assert!(present, "{}.{} must exist after migration", table, col);
    }

    for index in [
        "idx_proxy_logs_project_trace",
        "idx_proxy_logs_error_group",
        "idx_revenue_events_project_occurred",
        "idx_error_events_project_trace",
    ] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = '{}') AS present",
                    index
                ),
            ))
            .await?
            .expect("index query returns one row");
        let present: bool = row.try_get("", "present")?;
        assert!(present, "index {} must exist after migration", index);
    }

    // ── 6. The current migration relies on hypertable-atomic
    //       `ADD COLUMN IF NOT EXISTS` instead of the old per-chunk
    //       decompress dance (see m20260502's header for why that was
    //       abandoned after the orphan-chunk incident). Pin that contract:
    //       compressed chunks must have survived the migration untouched —
    //       if this ever starts decompressing again, that's a regression
    //       back toward the v1 approach and needs a deliberate decision.
    let post = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) FILTER (WHERE is_compressed) AS compressed \
             FROM timescaledb_information.chunks WHERE hypertable_name = 'proxy_logs'"
                .to_string(),
        ))
        .await?
        .expect("post chunk count");
    let compressed_after: i64 = post.try_get("", "compressed")?;
    assert!(
        compressed_after > 0,
        "expected compressed proxy_logs chunks to survive the migration \
         (the ALTER path must not decompress); got {} compressed after, {} before",
        compressed_after,
        compressed_before
    );

    // ── 7. Compression policy must be restored. ────────────────────────────
    let policy_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) AS n FROM timescaledb_information.jobs \
             WHERE hypertable_name = 'proxy_logs' AND application_name LIKE 'Columnstore%'"
                .to_string(),
        ))
        .await?
        .expect("policy count");
    let policy_count: i64 = policy_row.try_get("", "n")?;
    assert_eq!(
        policy_count, 1,
        "compression policy must be re-added after migration (got {})",
        policy_count
    );

    // ── 8. Data must be intact. ────────────────────────────────────────────
    let row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) AS n FROM proxy_logs".to_string(),
        ))
        .await?
        .expect("row count");
    let row_count: i64 = row.try_get("", "n")?;
    assert_eq!(
        row_count, 30,
        "all 30 backfilled rows must survive the decompress-alter-recompress cycle"
    );

    println!(
        "✅ observe correlation migration succeeded with {} compressed chunks before",
        compressed_before
    );
    Ok(())
}

/// Idempotency guard: re-running the observe correlation migration on an
/// already-migrated DB must succeed silently. Catches regressions where
/// someone replaces an `IF NOT EXISTS` with a plain ALTER.
#[tokio::test]
async fn test_observe_correlation_migration_is_idempotent() -> anyhow::Result<()> {
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_observe_correlation_migration_is_idempotent: \
             external database in use"
        );
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    Migrator::up(&db, None).await?;

    // Strip the recorded migration row so Sea-ORM thinks it needs to run
    // again; the migration body itself must still be a no-op.
    db.execute_unprepared(
        "DELETE FROM seaql_migrations WHERE version = 'm20260502_000001_add_observe_correlation'",
    )
    .await?;

    // Should succeed without error — every step uses IF NOT EXISTS / if_exists.
    Migrator::up(&db, None).await?;

    println!("✅ observe correlation migration is idempotent");
    Ok(())
}

// ---------------------------------------------------------------------------
// Reproduction: the actual prod failure. The original "chunk not found" error
// comes from a race between two concurrent operations on the same hypertable
// — typically the migration's `decompress_chunk(c)` enumerated via
// `show_chunks()` racing with the background retention worker that drops
// chunks older than 30 days. By the time `decompress_chunk(stale_oid)`
// runs, the chunk has already been dropped, and TimescaleDB throws.
//
// This test reconstructs the race deterministically by running retention in
// a tight loop in a background tokio task while the migration executes in
// the foreground. The fixed migration must complete cleanly even though
// retention is constantly mutating chunk metadata.
//
// To verify this test catches the regression, replace the v4 migration's
// `alter_job(scheduled => false)` with a no-op — the test must then fail.
#[tokio::test]
async fn test_observe_correlation_migration_survives_concurrent_retention() -> anyhow::Result<()> {
    if external_db_configured() {
        println!(
            "⏭️  Skipping test_observe_correlation_migration_survives_concurrent_retention: \
             external database in use"
        );
        return Ok(());
    }

    let container = GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
        .with_cmd(vec![
            "postgres",
            "-c",
            "timescaledb.max_background_workers=0",
        ])
        .start()
        .await
        .expect("Failed to start TimescaleDB container");
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    // Bring schema up to but not including the target migration.
    let target = "m20260502_000001_add_observe_correlation";
    let pre_target_count = Migrator::migrations()
        .iter()
        .position(|m| m.name() == target)
        .unwrap_or_else(|| panic!("migration {} not found in Migrator", target));
    Migrator::up(&db, Some(pre_target_count as u32)).await?;

    // Backfill `proxy_logs` with rows spanning 1d → 35d so retention can
    // actually drop something on each run, AND so we have plenty of
    // compressed chunks the migration must decompress.
    db.execute_unprepared(
        "INSERT INTO proxy_logs ( \
             timestamp, method, path, host, status_code, request_source, \
             is_system_request, routing_status, request_id, created_date \
         ) \
         SELECT \
             now() - (s * INTERVAL '1 day') - (i * INTERVAL '1 hour'), \
             'GET', '/api/r/' || s || '/' || i, 'example.test', 200, 'proxy', \
             false, 'routed', 'r-' || s || '-' || i, \
             (now() - (s * INTERVAL '1 day') - (i * INTERVAL '1 hour'))::date \
         FROM generate_series(1, 35) s, generate_series(1, 4) i",
    )
    .await?;
    db.execute_unprepared(
        "SELECT compress_chunk(c, if_not_compressed => TRUE) \
         FROM show_chunks('proxy_logs', older_than => now() - INTERVAL '7 days') c",
    )
    .await?;

    let pre = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) FILTER (WHERE is_compressed) AS compressed, count(*) AS total \
             FROM timescaledb_information.chunks WHERE hypertable_name = 'proxy_logs'"
                .to_string(),
        ))
        .await?
        .expect("chunk count");
    let compressed_before: i64 = pre.try_get("", "compressed")?;
    let total_before: i64 = pre.try_get("", "total")?;
    assert!(
        compressed_before > 0 && total_before > 5,
        "test setup must produce both compressed and uncompressed chunks \
         (got {} compressed / {} total) — race surface is too small otherwise",
        compressed_before,
        total_before
    );

    // Spawn a background task that hammers retention. We need a separate
    // connection because the main connection will hold an exclusive lock
    // on `proxy_logs` while the migration runs. The retention CALL will
    // block on that lock — exactly what we want.
    let bg_url = db_url.clone();
    let bg_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bg_stop_clone = bg_stop.clone();
    let bg_handle = tokio::spawn(async move {
        let bg_db = sea_orm::Database::connect(&bg_url).await?;
        // Find the retention job id once.
        let row = bg_db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT job_id FROM timescaledb_information.jobs \
                 WHERE hypertable_name = 'proxy_logs' \
                   AND proc_name = 'policy_retention' LIMIT 1"
                    .to_string(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("retention job not found"))?;
        let job_id: i32 = row.try_get("", "job_id")?;
        let call_sql = format!("CALL run_job({})", job_id);

        while !bg_stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            // We don't care about the result — the worker may legitimately
            // fail when the migration has it locked or when the job_id was
            // briefly removed during alter_job. We're only here to maximize
            // the chance of a race.
            let _ = bg_db.execute_unprepared(&call_sql).await;
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        }
        Ok::<(), anyhow::Error>(())
    });

    // Now run the migration. Must succeed despite the retention worker
    // hammering the same hypertable.
    let migration_result = Migrator::up(&db, None).await;

    // Stop and join the retention thrasher.
    bg_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bg_handle.await;

    migration_result?;

    // Schema must be intact post-race.
    for (table, col) in [
        ("proxy_logs", "trace_id"),
        ("proxy_logs", "error_group_id"),
        ("revenue_events", "deployment_id"),
        ("error_events", "trace_id_indexed"),
    ] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                     WHERE table_name = '{}' AND column_name = '{}') AS present",
                    table, col
                ),
            ))
            .await?
            .expect("col query returns one row");
        let present: bool = row.try_get("", "present")?;
        assert!(present, "{}.{} must exist after migration", table, col);
    }

    // Background jobs must be re-enabled — the migration paused them and
    // forgetting to restore would silently disable compression / retention
    // on prod.
    let jobs_row = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*) FILTER (WHERE scheduled) AS active, count(*) AS total \
             FROM timescaledb_information.jobs WHERE hypertable_name = 'proxy_logs'"
                .to_string(),
        ))
        .await?
        .expect("jobs query");
    let active: i64 = jobs_row.try_get("", "active")?;
    let total_jobs: i64 = jobs_row.try_get("", "total")?;
    assert_eq!(
        active, total_jobs,
        "every TimescaleDB job that was active before the migration must be \
         active after (got {} active / {} total)",
        active, total_jobs
    );

    println!(
        "✅ migration survived concurrent retention (started with {} compressed / {} total chunks)",
        compressed_before, total_before
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Regression test: the MFA session-purpose migration must fail closed.
// Pre-upgrade rows are ambiguous, so all are revoked. An old binary may still
// omit `mfa_pending` during a rolling upgrade; the database default must mark
// such rows pending rather than authenticate them.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_mfa_pending_migration_revokes_ambiguous_sessions_and_defaults_closed(
) -> anyhow::Result<()> {
    if external_db_configured() {
        println!("⏭️  Skipping MFA session migration test: external database in use");
        return Ok(());
    }

    let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        // Same fix as TestDatabase (#196) and CI's shared container: the
        // TimescaleDB background-worker launcher polls independently of the
        // test and can compress/drop chunks mid-test ("chunk not found").
        // Disabling background workers kills that scheduler race; tests
        // that deliberately race jobs (concurrent-retention) still work,
        // because `CALL run_job(...)` executes in-session, not via the
        // launcher.
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
            println!("⏭️  Skipping MFA session migration test: {error}");
            return Ok(());
        }
    };
    let port = container.get_host_port_ipv4(5432).await?;
    let db_url = format!("postgresql://postgres:postgres@localhost:{port}/postgres");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    let db = connect_with_retries(&db_url).await?;

    let target = "m20260713_000001_add_mfa_pending_to_sessions";
    let pre_target_count = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == target)
        .unwrap_or_else(|| panic!("migration {target} not found in Migrator"));
    Migrator::up(&db, Some(pre_target_count as u32)).await?;

    let user = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO users (name, email, created_at, updated_at) \
             VALUES ('MFA migration user', 'mfa-migration@example.test', now(), now()) \
             RETURNING id"
                .to_string(),
        ))
        .await?
        .expect("inserted user row");
    let user_id: i32 = user.try_get("", "id")?;

    db.execute_unprepared(&format!(
        "INSERT INTO sessions (user_id, session_token, expires_at) VALUES \
         ({user_id}, 'pre-upgrade-real', now() + INTERVAL '7 days'), \
         ({user_id}, 'pre-upgrade-challenge', now() + INTERVAL '5 minutes')"
    ))
    .await?;

    Migrator::up(&db, None).await?;

    let remaining = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT count(*)::int AS count FROM sessions".to_string(),
        ))
        .await?
        .expect("session count row");
    let remaining_count: i32 = remaining.try_get("", "count")?;
    assert_eq!(
        remaining_count, 0,
        "all ambiguous pre-upgrade sessions must be revoked"
    );

    let column = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'sessions' \
               AND column_name = 'mfa_pending'"
                .to_string(),
        ))
        .await?
        .expect("mfa_pending schema row");
    let is_nullable: String = column.try_get("", "is_nullable")?;
    let column_default: String = column.try_get("", "column_default")?;
    assert_eq!(is_nullable, "NO", "mfa_pending must remain mandatory");
    assert_eq!(
        column_default, "true",
        "omitted session purpose must default to MFA-pending"
    );

    let legacy_insert = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "INSERT INTO sessions (user_id, session_token, expires_at) \
                 VALUES ({user_id}, 'mixed-version-challenge', now() + INTERVAL '5 minutes') \
                 RETURNING mfa_pending"
            ),
        ))
        .await?
        .expect("legacy-style session insert");
    let defaults_pending: bool = legacy_insert.try_get("", "mfa_pending")?;
    assert!(
        defaults_pending,
        "a mixed-version insert that omits purpose must fail closed"
    );

    db.execute_unprepared(&format!(
        "INSERT INTO sessions (user_id, session_token, expires_at, mfa_pending) \
         VALUES ({user_id}, 'new-real-session', now() + INTERVAL '7 days', FALSE)"
    ))
    .await?;

    Ok(())
}
