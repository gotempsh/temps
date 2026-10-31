// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test utilities for database integration tests
//!
//! This module provides reusable test utilities for setting up
//! PostgreSQL with TimescaleDB for integration testing across
//! all temps crates.
//!
//! # Usage Modes
//!
//! ## 1. Shared Container (Default - Recommended)
//! ```rust,no_run
//! use temps_database::test_utils::TestDatabase;
//!
//! #[tokio::test]
//! async fn my_test() -> anyhow::Result<()> {
//!     let test_db = TestDatabase::new().await?;
//!     // Each test gets a unique schema for isolation
//!     // Container is shared across all tests in the run
//!     Ok(())
//! }
//! ```
//!
//! ## 2. External Database (via Environment Variable)
//! Set `TEMPS_TEST_DATABASE_URL` to connect to an existing database:
//! ```bash
//! export TEMPS_TEST_DATABASE_URL="postgresql://user:pass@localhost:5432/testdb"
//! cargo test
//! ```
//!
//! This is useful for:
//! - CI/CD environments with existing database services
//! - Local development with a persistent test database
//! - Faster test execution (no container startup time)
//!
//! ## 3. Isolated Container
//! ```rust,no_run
//! use temps_database::test_utils::TestDatabase;
//!
//! #[tokio::test]
//! async fn my_isolated_test() -> anyhow::Result<()> {
//!     // Creates a dedicated container (not shared)
//!     let test_db = TestDatabase::new_isolated().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Features
//!
//! - **Automatic schema isolation**: Each test gets a unique schema
//! - **Parallel test execution**: Tests can run concurrently without conflicts
//! - **Automatic cleanup**: Schemas are dropped when tests complete
//! - **Flexible modes**: Container-based or external database
//! - **Migration support**: Built-in support for running migrations

use crate::DbConnection;
use sea_orm::*;
use sea_orm_migration::MigratorTrait;
use std::sync::Arc;
use temps_migrations::Migrator;
use testcontainers::{core::WaitFor, runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{Mutex, OnceCell};

/// The postgres/timescaledb entrypoint starts a temporary server to run
/// initdb + init scripts, shuts it down, then starts the real server --
/// "database system is ready to accept connections" is logged once for
/// each.
///
/// The two go to DIFFERENT streams. Verified against
/// `timescale/timescaledb-ha:pg18`, the temporary server logs to **stdout**
/// (`[40] LOG: ...`) and the real server to **stderr** (`[1] LOG: ...`).
/// Matching on stderr therefore already targets the real server -- this does
/// not need a trailing buffer sleep to "clear the restart window", and it must
/// not be changed to match stdout or to wait for two occurrences, because each
/// stream only ever carries one.
///
/// Same wait condition already proven reliable elsewhere in this workspace
/// (temps-providers/src/pg_stat_statements.rs,
/// temps-metrics/tests/postgres_checkpoint_stats_integration.rs).
fn postgres_ready_wait_for() -> WaitFor {
    WaitFor::message_on_stderr("database system is ready to accept connections")
}

/// Returns true only for errors that indicate the container runtime itself
/// cannot be reached. Test callers may skip on these infrastructure errors,
/// while migration, schema, and image failures must still fail the test.
pub fn is_container_runtime_unavailable(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    [
        "hyper legacy client: client error (connect)",
        "failed to connect to docker",
        "error connecting to docker",
        "docker daemon is unavailable",
        "docker client is unavailable",
        "could not find docker environment",
        "docker socket",
        // testcontainers when no runtime is installed or running, e.g.
        // "failed to initialize a docker client: Socket not found: /var/run/docker.sock"
        // (also Colima/Rancher Desktop without DOCKER_HOST set). A bare
        // "socket not found" is deliberately not a marker: it is only a
        // missing runtime when it names a Docker socket, see
        // `is_missing_docker_socket`.
        "failed to initialize a docker client",
        "cannot connect to the docker daemon",
        "is the docker daemon running",
    ]
    .iter()
    .any(|marker| message.contains(marker))
        || is_missing_docker_socket(&message)
}

/// "Socket not found" / "No such file or directory" only mean the container
/// runtime is missing when the message also names a Docker socket path, so a
/// missing migration file or unrelated socket still fails the test.
fn is_missing_docker_socket(message: &str) -> bool {
    let names_docker_socket = message.contains("docker.sock") || message.contains("docker socket");
    names_docker_socket
        && (message.contains("socket not found")
            || message.contains("no such file or directory")
            || message.contains("connection refused")
            || message.contains("permission denied"))
}

/// Shared test database container that lives for the duration of the test run
static TEST_CONTAINER: OnceCell<Arc<Mutex<Option<SharedContainer>>>> = OnceCell::const_new();

/// Reference counter for active TestDatabase instances using the shared container
static ACTIVE_INSTANCES: OnceCell<Arc<Mutex<usize>>> = OnceCell::const_new();

/// Global migration lock to ensure only one test runs migrations at a time
/// This prevents race conditions when multiple tests try to create TimescaleDB
/// continuous aggregates and internal types simultaneously
static MIGRATION_LOCK: OnceCell<Arc<Mutex<()>>> = OnceCell::const_new();

/// Retries a migration-apply closure on transient Postgres concurrency errors
/// (deadlock_detected `40P01`, serialization_failure `40001`). `MIGRATION_LOCK`
/// above only serializes migrations within one test *process* — CI runs many
/// crates' test binaries as separate OS processes against the same shared
/// TimescaleDB instance, so their hypertable/continuous-aggregate DDL can
/// still race across processes and get killed by Postgres's deadlock
/// detector. That's expected and safe to retry: the losing transaction was
/// cleanly aborted, not left in a bad state.
async fn run_migrations_with_retry<F, Fut>(mut run: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), sea_orm::DbErr>>,
{
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        match run().await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < MAX_ATTEMPTS && is_transient_migration_error(&e.to_string()) => {
                let backoff = std::time::Duration::from_millis(100 * attempt as u64);
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(anyhow::anyhow!("Failed to run migrations: {}", e)),
        }
    }
    unreachable!("loop always returns Ok or Err before exhausting MAX_ATTEMPTS")
}

fn is_transient_migration_error(message: &str) -> bool {
    message.contains("deadlock detected")
        || message.contains("40P01")
        || message.contains("could not serialize access")
        || message.contains("40001")
}

/// Shared container wrapper that holds the database container and connection details
struct SharedContainer {
    #[allow(dead_code)] // held so the container is stopped on drop
    container: ContainerAsync<GenericImage>,
    database_url: String,
    #[allow(dead_code)] // kept with the container it describes
    port: u16,
}

impl SharedContainer {
    async fn new() -> anyhow::Result<Self> {
        let db_name = "test_db";
        let username = "test_user";
        let password = "test_password";

        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_wait_for(postgres_ready_wait_for())
            .with_env_var("POSTGRES_DB", db_name)
            .with_env_var("POSTGRES_USER", username)
            .with_env_var("POSTGRES_PASSWORD", password)
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd(TIMESCALEDB_NO_BACKGROUND_WORKERS_CMD)
            // testcontainers' default is 60s. CI and dev boxes here routinely run
            // many Docker-based integration tests concurrently (this repo's own
            // convention), so the container can legitimately take longer than
            // that to become ready under contention -- give it real headroom
            // instead of racing the default.
            .with_startup_timeout(std::time::Duration::from_secs(120))
            .start()
            .await?;

        // Get connection details
        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, db_name
        );

        // The wait strategy above already fires on the REAL server's "ready"
        // line (see `postgres_ready_wait_for`), so this is not closing a
        // restart window -- it is just a small margin for the mapped port to
        // start accepting. Callers retry on top of it.
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        Ok(Self {
            container: postgres_container,
            database_url,
            port,
        })
    }
}

/// Every migration-driven test creates its own hypertables/continuous
/// aggregates (with refresh policies) in a fresh, short-lived schema.
/// TimescaleDB's background worker launcher polls for due jobs every
/// ~60s (`timescaledb.bgw_launcher_poll_time`) and, if its scan happens to
/// land while one of those schemas is still alive, launches a real
/// background worker to refresh it -- a second Postgres backend racing the
/// test's own foreground transaction. That's a plausible root cause of the
/// Postgres deadlocks (40P01) observed even under full test serialization
/// (confirmed: a background job did execute mid-test-run against a fresh
/// container). Tests never rely on a background refresh actually firing
/// (job timing isn't something a test could deterministically wait on
/// anyway), so disabling the launcher entirely removes the race with no
/// loss of coverage. This only affects test containers, not migrations
/// themselves -- `max_background_workers=0` still allows `CREATE
/// EXTENSION`/`create_hypertable`/`add_continuous_aggregate_policy` to
/// succeed, it just means no worker ever executes the scheduled jobs.
const TIMESCALEDB_NO_BACKGROUND_WORKERS_CMD: [&str; 3] =
    ["postgres", "-c", "timescaledb.max_background_workers=0"];

// ---------------------------------------------------------------------------
// Per-test databases cloned from a migrated template
// ---------------------------------------------------------------------------
//
// `with_migrations()` used to replay every migration (300+, including
// hypertables and continuous aggregates) into a fresh schema of one shared
// database, for every test. That made it the dominant cost of the Rust test
// suite, and because all those schemas share one database's catalog, their
// DDL deadlocked across test processes, which is why CI had to run every
// database-backed test with `--test-threads=1`.
//
// Instead, the migrations run once into a template database, and each test
// gets `CREATE DATABASE ... TEMPLATE` -- a file-level copy that takes a
// fraction of a second and shares no catalog with any other test. One
// template is built per test run (nextest's `NEXTEST_RUN_ID`, or this process
// under plain `cargo test`), under a Postgres advisory lock so concurrent test
// processes wait for the first one instead of all migrating at once.
//
// A server that refuses `CREATE DATABASE` (an external
// `TEMPS_TEST_DATABASE_URL` pointing at a role without `CREATEDB`) falls back
// to the old schema-per-test path, so nothing that worked before stops working.

/// Prefix of every per-test database cloned from a template.
const TEST_DATABASE_PREFIX: &str = "temps_test_";

/// Prefix of every migrated template database.
const TEMPLATE_DATABASE_PREFIX: &str = "temps_tpl_";

/// `COMMENT ON DATABASE` marker recording when a test or template database
/// was created, so leftovers of killed test processes can be swept.
const CREATED_AT_COMMENT_PREFIX: &str = "temps-test-db created_at=";

/// Test and template databases older than this are dropped the next time a
/// template is built. Only matters for long-lived servers (a local
/// `TEMPS_TEST_DATABASE_URL`); CI throws its database away with the job.
const STALE_TEST_DATABASE_SECS: u64 = 6 * 60 * 60;

/// How long a test process waits for another one to finish building the
/// template (and for any other lock on the maintenance connection) before
/// failing. Well above a normal template build, and below nextest's
/// 10-minute kill, so a stuck builder fails its waiters with an error that
/// names the template instead of a bare `TIMEOUT`.
const TEMPLATE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Identifies this test run, so every process of one nextest run shares one
/// template and no run can reuse a template built from different migrations.
#[allow(
    clippy::disallowed_methods,
    reason = "test/dev-only toggle: NEXTEST_RUN_ID is set by the test runner; this module is test support only"
)]
fn test_run_scope() -> &'static str {
    static SCOPE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SCOPE.get_or_init(|| match std::env::var("NEXTEST_RUN_ID") {
        Ok(run_id) if !run_id.trim().is_empty() => run_id,
        _ => {
            let started = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            format!("pid-{}-{}", std::process::id(), started)
        }
    })
}

/// Name of the template database holding `M`'s migrations for this run.
fn template_database_name<M: MigratorTrait>(run_scope: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::any::type_name::<M>().hash(&mut hasher);
    for migration in M::migrations() {
        migration.name().hash(&mut hasher);
    }
    run_scope.hash(&mut hasher);
    format!("{}{:016x}", TEMPLATE_DATABASE_PREFIX, hasher.finish())
}

/// `TEMPS_TEST_DATABASE_URL`, treating an empty value as unset: GitHub
/// Actions cannot omit a variable conditionally, only empty it.
#[allow(
    clippy::disallowed_methods,
    reason = "test/dev-only toggle: TEMPS_TEST_DATABASE_URL points test support at an external database"
)]
fn external_database_url() -> Option<String> {
    std::env::var("TEMPS_TEST_DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Replaces the database name in a `postgresql://` URL, keeping its query
/// string (`sslmode` and the like apply to every database on the server).
fn with_database_name(url: &str, database: &str) -> anyhow::Result<String> {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let authority_start = base
        .find("://")
        .map(|index| index + 3)
        .ok_or_else(|| anyhow::anyhow!("Test database URL has no scheme: {}", base))?;
    let authority_end = base[authority_start..]
        .find('/')
        .map(|index| authority_start + index)
        .unwrap_or(base.len());
    Ok(match query {
        Some(query) => format!("{}/{}?{}", &base[..authority_end], database, query),
        None => format!("{}/{}", &base[..authority_end], database),
    })
}

/// The URL a cloned test database is handed out under. It ends in an
/// `options=-c ...` parameter, like the schema path's URLs always have, so
/// tests that tune their own pool by appending ` -c name=value` to
/// `database_url` keep working: without it the appended text would land in
/// the database name.
fn cloned_database_url(server_url: &str, database: &str) -> anyhow::Result<String> {
    let url = with_database_name(server_url, database)?;
    let separator = if url.contains('?') { '&' } else { '?' };
    Ok(format!("{url}{separator}options=-c search_path=public"))
}

/// Whether `CREATE DATABASE` failed because the role may not create
/// databases, as opposed to the server being broken.
fn is_create_database_denied(error: &str) -> bool {
    error.contains("permission denied to create database") || error.contains("42501")
}

/// A per-test database and the server database used to drop it again.
struct ClonedDatabase {
    name: String,
    server_url: String,
}

/// Why a per-test database could not be provided.
enum TemplateDatabaseError {
    /// The server does not let this role create databases; use schemas.
    Unsupported(String),
    /// Anything else, which must fail the test.
    Failed(anyhow::Error),
}

impl From<sea_orm::DbErr> for TemplateDatabaseError {
    fn from(error: sea_orm::DbErr) -> Self {
        TemplateDatabaseError::Failed(anyhow::anyhow!(error))
    }
}

impl From<anyhow::Error> for TemplateDatabaseError {
    fn from(error: anyhow::Error) -> Self {
        TemplateDatabaseError::Failed(error)
    }
}

/// One connection to the server's own database, for `CREATE`/`DROP
/// DATABASE`. A single connection, because the advisory lock it takes is
/// held by the session that took it.
async fn connect_maintenance(server_url: &str) -> anyhow::Result<DbConnection> {
    let options = ConnectOptions::new(server_url)
        .max_connections(1)
        .min_connections(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .acquire_timeout(std::time::Duration::from_secs(30))
        .sqlx_logging(false)
        .to_owned();
    Database::connect(options).await.map_err(|error| {
        anyhow::anyhow!(
            "Failed to connect to test database server for database management: {}",
            error
        )
    })
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

async fn mark_created_now(admin: &DbConnection, database: &str) -> Result<(), sea_orm::DbErr> {
    admin
        .execute_unprepared(&format!(
            "COMMENT ON DATABASE {} IS '{}{}'",
            database,
            CREATED_AT_COMMENT_PREFIX,
            unix_now()
        ))
        .await
        .map(|_| ())
}

/// Drops test and template databases left behind by killed test processes.
/// Best-effort: a failure here never fails a test.
///
/// The created-at cast sits inside a CASE because Postgres does not promise
/// to evaluate WHERE conditions in order, and other databases carry comments
/// of their own (`postgres` ships with one) that would fail a bare `::bigint`.
async fn sweep_stale_test_databases(admin: &DbConnection) {
    let cutoff = unix_now().saturating_sub(STALE_TEST_DATABASE_SECS) as i64;
    let stale = admin
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT d.datname, d.datistemplate FROM pg_database d \
             JOIN pg_shdescription s \
               ON s.objoid = d.oid AND s.classoid = 'pg_database'::regclass \
             WHERE (starts_with(d.datname, $1) OR starts_with(d.datname, $2)) \
               AND CASE \
                     WHEN starts_with(s.description, $3) \
                      AND substr(s.description, length($3) + 1) ~ '^[0-9]+$' \
                     THEN substr(s.description, length($3) + 1)::bigint < $4 \
                     ELSE false \
                   END",
            [
                TEST_DATABASE_PREFIX.into(),
                TEMPLATE_DATABASE_PREFIX.into(),
                CREATED_AT_COMMENT_PREFIX.into(),
                cutoff.into(),
            ],
        ))
        .await;
    let rows = match stale {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("Warning: Could not list stale test databases: {}", error);
            return;
        }
    };
    for row in rows {
        let Ok(name) = row.try_get::<String>("", "datname") else {
            continue;
        };
        if row.try_get::<bool>("", "datistemplate").unwrap_or(false) {
            let _ = admin
                .execute_unprepared(&format!("ALTER DATABASE {} IS_TEMPLATE false", name))
                .await;
        }
        if let Err(error) = admin
            .execute_unprepared(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", name))
            .await
        {
            eprintln!(
                "Warning: Failed to drop stale test database {}: {}",
                name, error
            );
        }
    }
}

/// Makes sure the template for `M` exists and is fully migrated. Runs under
/// an advisory lock on `admin`'s session, so only one process builds it.
async fn ensure_template_database<M: MigratorTrait>(
    admin: &DbConnection,
    server_url: &str,
    template: &str,
) -> Result<(), TemplateDatabaseError> {
    let ready = admin
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT datistemplate FROM pg_database WHERE datname = $1",
            [template.into()],
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "datistemplate").unwrap_or(false));
    match ready {
        Some(true) => return Ok(()),
        // Created but never marked ready: a process died mid-migration.
        Some(false) => {
            admin
                .execute_unprepared(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    template
                ))
                .await?;
        }
        None => {}
    }

    sweep_stale_test_databases(admin).await;

    if let Err(error) = admin
        .execute_unprepared(&format!("CREATE DATABASE {}", template))
        .await
    {
        let message = error.to_string();
        if is_create_database_denied(&message) {
            return Err(TemplateDatabaseError::Unsupported(message));
        }
        return Err(TemplateDatabaseError::Failed(anyhow::anyhow!(
            "Failed to create template database {}: {}",
            template,
            message
        )));
    }
    mark_created_now(admin, template).await?;

    let template_url = with_database_name(server_url, template)?;
    let template_db = TestDatabase::connect_with_retry(&template_url, 5).await?;
    let migrated = async {
        // Extensions are optional on some servers (pgvector), exactly as on
        // the schema path, so a missing one is not an error here.
        template_db
            .execute_unprepared(
                "CREATE EXTENSION IF NOT EXISTS timescaledb WITH SCHEMA public CASCADE",
            )
            .await
            .ok();
        template_db
            .execute_unprepared("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public CASCADE")
            .await
            .ok();
        run_migrations_with_retry(|| M::up(&template_db, None))
            .await
            .map_err(|error| anyhow::anyhow!("Template database {}: {}", template, error))
    }
    .await;
    // Every connection to a template must be gone before it can be cloned.
    let _ = template_db.close().await;
    migrated?;

    // ALLOW_CONNECTIONS false keeps a stray connection from blocking every
    // clone ("source database is being accessed by other users").
    admin
        .execute_unprepared(&format!(
            "ALTER DATABASE {} WITH IS_TEMPLATE true ALLOW_CONNECTIONS false",
            template
        ))
        .await?;
    Ok(())
}

/// Test database setup with TimescaleDB container
pub struct TestDatabase {
    pub db: Arc<DbConnection>,
    pub database_url: String,
    /// If this instance owns a dedicated container (not shared)
    #[allow(dead_code)] // held so the container is stopped on drop
    dedicated_container: Option<ContainerAsync<GenericImage>>,
    /// The unique schema name for this test instance
    schema_name: Option<String>,
    /// The database of its own this test instance was cloned into from a
    /// migrated template, dropped on cleanup
    database_name: Option<ClonedDatabase>,
    /// Whether this instance is using the shared container
    uses_shared_container: bool,
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        // Skip cleanup if already cleaned up via explicit cleanup() call
        let has_schema = self.schema_name.is_some();
        let has_database = self.database_name.is_some();
        let needs_counter_decrement = self.uses_shared_container;

        if !has_schema && !has_database && !needs_counter_decrement {
            return;
        }

        // Run async cleanup synchronously from Drop.
        //
        // Strategy:
        //   1. Try block_in_place + block_on (works on multi-threaded tokio runtime)
        //      using the existing SeaORM connection pool
        //   2. If that panics (single-threaded runtime), fall back to spawning a
        //      dedicated thread with its own runtime and a FRESH database connection
        //      (the SeaORM pool is tied to the original runtime and won't work)
        //   3. If all else fails, log the leaked schema
        //
        // Previous approach used tokio::spawn which was fire-and-forget — spawned
        // tasks would race against runtime shutdown, leaving orphaned schemas.
        let schema = self.schema_name.take();
        let database = self.database_name.take();
        let database_url = self.database_url.clone();

        let cleanup = move || {
            // Always use a dedicated thread for cleanup. This approach works on both
            // multi-threaded and single-threaded (current_thread) tokio runtimes without
            // panicking. The previous approach used block_in_place which panics on
            // current_thread runtime, and even though catch_unwind caught the panic,
            // the default panic hook would print scary messages to stderr that could
            // confuse CI systems and test runners.
            let schema2 = schema.clone();
            let cleanup_thread = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    rt.block_on(async {
                        if let Some(ref database) = database {
                            Self::drop_test_database(database).await;
                        }
                        Self::run_cleanup_with_fresh_connection(
                            &database_url,
                            &schema2,
                            needs_counter_decrement,
                        )
                        .await;
                    });
                } else if let Some(ref s) = schema2 {
                    eprintln!(
                        "Warning: Cannot clean up test schema {} (failed to create runtime)",
                        s
                    );
                }
            });
            let _ = cleanup_thread.join();
        };

        // Wrap in catch_unwind so panics in Drop don't abort the process
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        if result.is_err() {
            eprintln!("Warning: TestDatabase cleanup panicked (runtime may be shutting down)");
        }

        // Note: dedicated_container will be automatically cleaned up by testcontainers
        // when it goes out of scope (Drop is implemented by testcontainers)
    }
}

/// Longest a test database's cleanup waits for a lock on its schema.
///
/// `Drop` runs cleanup on a blocked thread and joins it, so an unbounded
/// `DROP SCHEMA ... CASCADE` that queues behind a lock held by a connection
/// of the same test (one left idle in a transaction) hangs the test process
/// with no output: the holder's own rollback can only run once this returns.
/// The schema belongs to a throwaway test run, so giving up after a warning
/// leaks a few empty tables at worst.
const SCHEMA_CLEANUP_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

impl TestDatabase {
    /// `DROP SCHEMA ... CASCADE` that gives up after `lock_timeout` instead
    /// of waiting on a lock indefinitely. `SET LOCAL` scopes the timeout to
    /// this one transaction, so the connection is returned unchanged.
    async fn drop_schema_bounded<C>(
        conn: &C,
        schema_name: &str,
        lock_timeout: std::time::Duration,
    ) -> Result<(), sea_orm::DbErr>
    where
        C: ConnectionTrait + sea_orm::TransactionTrait,
    {
        let txn = conn.begin().await?;
        let result = async {
            txn.execute_unprepared(&format!(
                "SET LOCAL lock_timeout = '{}ms'",
                lock_timeout.as_millis().max(1)
            ))
            .await?;
            txn.execute_unprepared(&format!("DROP SCHEMA IF EXISTS {} CASCADE", schema_name))
                .await
        }
        .await;
        match result {
            Ok(_) => txn.commit().await,
            Err(error) => {
                // Roll back now rather than at Drop, which only queues it.
                let _ = txn.rollback().await;
                Err(error)
            }
        }
    }

    /// Internal cleanup logic shared between Drop and the async cleanup() method.
    /// Uses the existing SeaORM connection pool.
    async fn run_cleanup(
        db: &Arc<DbConnection>,
        schema: &Option<String>,
        needs_counter_decrement: bool,
    ) {
        if let Some(schema_name) = schema {
            if let Err(e) =
                Self::drop_schema_bounded(db.as_ref(), schema_name, SCHEMA_CLEANUP_LOCK_TIMEOUT)
                    .await
            {
                eprintln!("Warning: Failed to drop test schema {}: {}", schema_name, e);
            }
        }

        Self::maybe_drop_shared_container(needs_counter_decrement).await;
    }

    /// Cleanup logic for the fallback thread path (single-threaded runtime).
    /// Creates a fresh database connection since the original SeaORM pool
    /// is tied to the test's runtime and won't work on a new runtime.
    async fn run_cleanup_with_fresh_connection(
        database_url: &str,
        schema: &Option<String>,
        needs_counter_decrement: bool,
    ) {
        if let Some(schema_name) = schema {
            // Extract the base URL (strip schema-specific query params)
            let base_url = database_url.split('?').next().unwrap_or(database_url);

            // Create a minimal fresh connection just for the DROP SCHEMA
            let connect_options = ConnectOptions::new(base_url)
                .max_connections(1)
                .min_connections(0)
                .connect_timeout(std::time::Duration::from_secs(5))
                .to_owned();

            match Database::connect(connect_options).await {
                Ok(fresh_db) => {
                    if let Err(e) = Self::drop_schema_bounded(
                        &fresh_db,
                        schema_name,
                        SCHEMA_CLEANUP_LOCK_TIMEOUT,
                    )
                    .await
                    {
                        eprintln!("Warning: Failed to drop test schema {}: {}", schema_name, e);
                    }
                    let _ = fresh_db.close().await;
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to connect for schema cleanup {}: {}",
                        schema_name, e
                    );
                }
            }
        }

        Self::maybe_drop_shared_container(needs_counter_decrement).await;
    }

    /// Decrement the active instances counter.
    ///
    /// The shared container is intentionally **not** dropped when the counter
    /// reaches zero. With `#[serial_test::serial]` (or any pattern where tests
    /// run one at a time), each test's `Drop` would destroy the container and
    /// the next test would have to spin up a brand-new one (5+ seconds each
    /// time). Instead we let the container live for the entire test process;
    /// testcontainers' own cleanup and the OS will tear it down when the
    /// process exits.
    async fn maybe_drop_shared_container(needs_counter_decrement: bool) {
        if needs_counter_decrement {
            if let Some(counter) = ACTIVE_INSTANCES.get() {
                let mut count = counter.lock().await;
                *count = count.saturating_sub(1);
            }
        }
    }
}

impl TestDatabase {
    /// Get or create the shared database container
    async fn get_or_create_container() -> anyhow::Result<Arc<Mutex<Option<SharedContainer>>>> {
        TEST_CONTAINER
            .get_or_try_init(|| async {
                let container = SharedContainer::new().await?;
                Ok(Arc::new(Mutex::new(Some(container))))
            })
            .await
            .map(Arc::clone)
    }

    /// Initialize or get the active instances counter
    async fn get_or_init_counter() -> Arc<Mutex<usize>> {
        ACTIVE_INSTANCES
            .get_or_init(|| async { Arc::new(Mutex::new(0)) })
            .await
            .clone()
    }

    /// Explicitly clean up the test schema (async version).
    ///
    /// This is called automatically by `Drop`, but can also be called
    /// explicitly for guaranteed cleanup in async test contexts.
    /// Calling this before the `TestDatabase` is dropped avoids relying
    /// on the synchronous `Drop` fallback.
    pub async fn cleanup(&mut self) {
        let schema = self.schema_name.take();
        let needs_counter = self.uses_shared_container;
        self.uses_shared_container = false; // Prevent double-decrement in Drop
        if let Some(database) = self.database_name.take() {
            // The test's own pool is still open; WITH (FORCE) in the drop
            // would terminate it, so close it first to keep its errors quiet.
            let _ = self.db.close_by_ref().await;
            Self::drop_test_database(&database).await;
        }
        Self::run_cleanup(&self.db, &schema, needs_counter).await;
    }

    /// Drops a database cloned from a template. `WITH (FORCE)` terminates
    /// the test's own connections, so a pool still open (or a connection left
    /// idle in a transaction) cannot hang the cleanup the way a lock-waiting
    /// `DROP SCHEMA` could.
    async fn drop_test_database(database: &ClonedDatabase) {
        let ClonedDatabase {
            name: database,
            server_url,
        } = database;
        match connect_maintenance(server_url).await {
            Ok(admin) => {
                if let Err(error) = admin
                    .execute_unprepared(&format!(
                        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                        database
                    ))
                    .await
                {
                    eprintln!(
                        "Warning: Failed to drop test database {}: {}",
                        database, error
                    );
                }
                let _ = admin.close().await;
            }
            Err(error) => {
                eprintln!(
                    "Warning: Failed to connect to drop test database {}: {}",
                    database, error
                );
            }
        }
    }

    /// Create a new test database with TimescaleDB (uses shared container or existing database)
    ///
    /// This function:
    /// 1. Checks for TEMPS_TEST_DATABASE_URL environment variable
    ///    - If set, connects to the existing database without creating a container
    /// 2. Otherwise, gets or creates a shared TimescaleDB container (only created once per test run)
    /// 3. Creates a unique schema for this test to ensure parallel test isolation
    /// 4. Establishes a connection with the unique schema in the search_path
    pub async fn new() -> anyhow::Result<Self> {
        // Check if TEMPS_TEST_DATABASE_URL is set
        if let Some(external_db_url) = external_database_url() {
            return Self::new_from_url(&external_db_url).await;
        }

        // Increment active instances counter
        let counter = Self::get_or_init_counter().await;
        {
            let mut count = counter.lock().await;
            *count += 1;
        }

        // Get or create shared container
        let container = Self::get_or_create_container().await?;
        let mut container_lock = container.lock().await;

        // Extract base_url from the Option<SharedContainer>, re-creating the
        // container if a previous test's Drop already took it. This handles the
        // case where serial tests each create and drop a TestDatabase: the first
        // test's cleanup sets the Option to None, and the next test needs a fresh
        // container.
        let base_url = match *container_lock {
            Some(ref shared_container) => shared_container.database_url.clone(),
            None => {
                let new_container = SharedContainer::new().await?;
                let url = new_container.database_url.clone();
                *container_lock = Some(new_container);
                url
            }
        };
        drop(container_lock); // Release lock early

        // Generate a unique schema name
        let schema_name = format!("s{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

        // Connect to default database to create schema
        let admin_db = Self::connect_with_retry(&base_url, 20).await?;

        // Create the unique schema
        let create_schema_sql = format!("CREATE SCHEMA IF NOT EXISTS {}", schema_name);
        let statement = Statement::from_string(DatabaseBackend::Postgres, create_schema_sql);
        let created = admin_db
            .execute(statement)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create test schema {}: {}", schema_name, e));
        // Only needed for the CREATE SCHEMA; left open it held a connection
        // (min_connections(1)) for the rest of the test.
        let _ = admin_db.close().await;
        created?;

        // Now reconnect with the schema in the search_path via connection string parameter
        // Include 'public' in search_path so TimescaleDB functions are accessible
        let database_url = format!("{}?options=-c search_path={},public", base_url, schema_name);

        // Connect with schema-specific search_path
        let db = Self::connect_with_retry_schema(&database_url, &schema_name, 20).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: None,
            schema_name: Some(schema_name),
            database_name: None,
            uses_shared_container: true,
        };

        // Verify connection works
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;

        Ok(test_db)
    }

    /// Create a test database from an existing database URL
    ///
    /// This connects to an existing PostgreSQL/TimescaleDB database without
    /// creating any containers. It creates a unique schema for test isolation.
    ///
    /// # Arguments
    /// * `database_url` - Full PostgreSQL connection string (e.g., postgresql://user:pass@localhost:5432/dbname)
    ///
    /// # Environment Variables
    /// This method is automatically used when `TEMPS_TEST_DATABASE_URL` is set.
    pub async fn new_from_url(database_url: &str) -> anyhow::Result<Self> {
        // Generate a unique schema name
        let schema_name = format!("s{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

        // Connect to the database to create schema
        let admin_db = Self::connect_with_retry(database_url, 20).await?;

        // Create the unique schema
        let create_schema_sql = format!("CREATE SCHEMA IF NOT EXISTS {}", schema_name);
        let statement = Statement::from_string(DatabaseBackend::Postgres, create_schema_sql);
        let created = admin_db
            .execute(statement)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create test schema {}: {}", schema_name, e));
        // Only needed for the CREATE SCHEMA; left open it held a connection
        // (min_connections(1)) for the rest of the test.
        let _ = admin_db.close().await;
        created?;

        // Reconnect with the schema in the search_path
        // Include 'public' in search_path so TimescaleDB functions are accessible
        let database_url_with_schema = format!(
            "{}?options=-c search_path={},public",
            database_url, schema_name
        );

        // Connect with schema-specific search_path
        let db =
            Self::connect_with_retry_schema(&database_url_with_schema, &schema_name, 20).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url: database_url_with_schema,
            dedicated_container: None,
            schema_name: Some(schema_name),
            database_name: None,
            uses_shared_container: false, // External DB, not using shared container
        };

        // Verify connection works
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;

        Ok(test_db)
    }

    /// Create a test database with a dedicated container (not shared)
    ///
    /// This creates a completely isolated database instance for tests that need
    /// full isolation and cannot share a container with other tests.
    /// Use this sparingly as it's slower than using the shared container.
    pub async fn new_isolated() -> anyhow::Result<Self> {
        Self::new_isolated_with_config("test_db", "test_user", "test_password").await
    }

    /// Create an isolated test database with custom configuration
    ///
    /// This creates a dedicated TimescaleDB container that is not shared with other tests.
    /// Useful for tests that need complete isolation or custom database configuration.
    pub async fn new_isolated_with_config(
        db_name: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Self> {
        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_wait_for(postgres_ready_wait_for())
            .with_env_var("POSTGRES_DB", db_name)
            .with_env_var("POSTGRES_USER", username)
            .with_env_var("POSTGRES_PASSWORD", password)
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd(TIMESCALEDB_NO_BACKGROUND_WORKERS_CMD)
            // testcontainers' default is 60s. CI and dev boxes here routinely run
            // many Docker-based integration tests concurrently (this repo's own
            // convention), so the container can legitimately take longer than
            // that to become ready under contention -- give it real headroom
            // instead of racing the default.
            .with_startup_timeout(std::time::Duration::from_secs(120))
            .start()
            .await?;

        // Get connection details
        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, db_name
        );

        // The wait strategy above already fires on the REAL server's "ready"
        // line (see `postgres_ready_wait_for`), so this is not closing a
        // restart window -- it is just a small margin for the mapped port to
        // start accepting.
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Connect with retries as a safety net on top of the margin above.
        let db = Self::connect_with_retry(&database_url, 10).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: Some(postgres_container),
            schema_name: None, // Isolated containers use public schema
            database_name: None,
            uses_shared_container: false,
        };

        // Verify connection works
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;
        // Run migrations after creating the isolated test database
        use sea_orm_migration::MigratorTrait;
        run_migrations_with_retry(|| temps_migrations::Migrator::up(&*test_db.db, None)).await?;
        Ok(test_db)
    }

    /// Create a test database and run migrations
    ///
    /// This is a convenience method that uses temps_migrations::Migrator.
    /// Each test gets a database of its own, cloned from a template that is
    /// migrated once per test run (see the note on `TEST_DATABASE_PREFIX`),
    /// so tests share no catalog and can run in parallel. Servers that refuse
    /// `CREATE DATABASE` get a fully migrated schema of their own instead.
    pub async fn with_migrations() -> anyhow::Result<Self> {
        let test_db = Self::migrated::<Migrator>().await?;

        // Verify migrations were successful by checking a known table in current schema
        let check_sql = "SELECT EXISTS (
            SELECT FROM information_schema.tables
            WHERE table_schema = current_schema()
            AND table_name = 'users'
        )";

        let result = test_db
            .query_sql(check_sql)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to verify migrations: {}", e))?;

        let users_table_exists = result
            .first()
            .and_then(|row| row.try_get::<bool>("", "exists").ok())
            .unwrap_or(false);

        if !users_table_exists {
            return Err(anyhow::anyhow!("Migrations did not create expected tables"));
        }

        Ok(test_db)
    }

    /// Create a test database and run migrations with custom Migrator
    ///
    /// This generic method allows using any MigratorTrait implementation.
    /// Like `with_migrations`, each test gets a database cloned from a
    /// template built once per run for `M`.
    pub async fn with_custom_migrations<M>() -> anyhow::Result<Self>
    where
        M: MigratorTrait,
    {
        Self::migrated::<M>().await
    }

    /// A database with `M`'s migrations applied: a clone of the run's
    /// template, or a migrated schema where databases cannot be created.
    async fn migrated<M: MigratorTrait>() -> anyhow::Result<Self> {
        static SCHEMA_FALLBACK: std::sync::OnceLock<()> = std::sync::OnceLock::new();

        if SCHEMA_FALLBACK.get().is_none() {
            let (server_url, uses_shared_container) = Self::server_url().await?;
            match Self::clone_from_template::<M>(&server_url).await {
                Ok(mut test_db) => {
                    test_db.uses_shared_container = uses_shared_container;
                    return Ok(test_db);
                }
                Err(TemplateDatabaseError::Unsupported(reason)) => {
                    if SCHEMA_FALLBACK.set(()).is_ok() {
                        eprintln!(
                            "Test database server does not allow CREATE DATABASE ({}); \
                             migrating a schema per test instead, which is much slower",
                            reason
                        );
                    }
                }
                Err(TemplateDatabaseError::Failed(error)) => return Err(error),
            }
        }

        Self::migrated_in_schema::<M>().await
    }

    /// The server tests create their databases on: `TEMPS_TEST_DATABASE_URL`,
    /// or the shared container (started on first use). The flag says whether
    /// the shared container's instance counter was incremented.
    async fn server_url() -> anyhow::Result<(String, bool)> {
        if let Some(url) = external_database_url() {
            return Ok((url, false));
        }

        let counter = Self::get_or_init_counter().await;
        *counter.lock().await += 1;

        let container = Self::get_or_create_container().await?;
        let mut container_lock = container.lock().await;
        let url = match *container_lock {
            Some(ref shared_container) => shared_container.database_url.clone(),
            None => {
                let new_container = SharedContainer::new().await?;
                let url = new_container.database_url.clone();
                *container_lock = Some(new_container);
                url
            }
        };
        Ok((url, true))
    }

    /// Clones `M`'s template for this run into a new database, building the
    /// template first if no process of this run has yet.
    async fn clone_from_template<M: MigratorTrait>(
        server_url: &str,
    ) -> Result<Self, TemplateDatabaseError> {
        let template = template_database_name::<M>(test_run_scope());
        let database = format!("{}{}", TEST_DATABASE_PREFIX, uuid::Uuid::new_v4().simple());

        let admin = connect_maintenance(server_url).await?;
        let created = async {
            // Session-level and keyed on the template, so concurrent test
            // processes queue here while the first one migrates. Released by
            // closing the connection below, whatever happened in between.
            // Session-level: `admin` is a single connection, closed below.
            admin
                .execute_unprepared(&format!(
                    "SET lock_timeout = '{}ms'",
                    TEMPLATE_LOCK_TIMEOUT.as_millis()
                ))
                .await?;
            admin
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT pg_advisory_lock(hashtextextended($1, 0))",
                    [template.clone().into()],
                ))
                .await
                .map_err(|error| {
                    TemplateDatabaseError::Failed(anyhow::anyhow!(
                        "Gave up after {:?} waiting for the lock on template {}; \
                         another test process is still building it or is stuck: {}",
                        TEMPLATE_LOCK_TIMEOUT,
                        template,
                        error
                    ))
                })?;
            ensure_template_database::<M>(&admin, server_url, &template).await?;
            admin
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                    [template.clone().into()],
                ))
                .await?;

            admin
                .execute_unprepared(&format!(
                    "CREATE DATABASE {} TEMPLATE {}",
                    database, template
                ))
                .await
                .map_err(|error| {
                    let message = error.to_string();
                    if is_create_database_denied(&message) {
                        TemplateDatabaseError::Unsupported(message)
                    } else {
                        TemplateDatabaseError::Failed(anyhow::anyhow!(
                            "Failed to clone template {} into test database {}: {}",
                            template,
                            database,
                            message
                        ))
                    }
                })?;
            mark_created_now(&admin, &database).await?;
            Ok::<(), TemplateDatabaseError>(())
        }
        .await;
        let _ = admin.close().await;
        created?;

        let cloned = ClonedDatabase {
            name: database.clone(),
            server_url: server_url.to_owned(),
        };
        let database_url = cloned_database_url(server_url, &database)?;
        let db = match Self::connect_with_retry(&database_url, 20).await {
            Ok(db) => db,
            Err(error) => {
                Self::drop_test_database(&cloned).await;
                return Err(TemplateDatabaseError::Failed(error));
            }
        };

        Ok(TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: None,
            schema_name: None,
            database_name: Some(cloned),
            uses_shared_container: false,
        })
    }

    /// The pre-template path: replays every migration of `M` into a fresh
    /// schema. Only used when the server refuses `CREATE DATABASE`.
    async fn migrated_in_schema<M: MigratorTrait>() -> anyhow::Result<Self> {
        let test_db = Self::new().await?;

        // Verify database connection is working
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Database connection test failed: {}", e))?;

        // Acquire the global migration lock to ensure only one test runs migrations at a time
        // This is critical for TimescaleDB which creates extensions, continuous aggregates,
        // and internal types that can conflict when created concurrently
        let migration_lock = MIGRATION_LOCK
            .get_or_init(|| async { Arc::new(Mutex::new(())) })
            .await;
        let _lock = migration_lock.lock().await;

        // Create extensions (protected by migration lock)
        // Extensions must be created in public schema (database-wide)
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS timescaledb WITH SCHEMA public CASCADE")
            .await
            .ok();
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public CASCADE")
            .await
            .ok();

        // Run migrations in this test's unique schema
        run_migrations_with_retry(|| M::up(&*test_db.db, None)).await?;

        Ok(test_db)
    }

    /// Connect to database with retry logic
    async fn connect_with_retry(
        database_url: &str,
        max_retries: u32,
    ) -> anyhow::Result<DbConnection> {
        use sea_orm::ConnectOptions;
        use std::time::Duration;

        let mut retries = max_retries;

        // Create connection options with better timeout settings
        let mut opt = ConnectOptions::new(database_url.to_owned());
        opt.max_connections(5)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(10))
            .max_lifetime(Duration::from_secs(60))
            .sqlx_logging(false);

        loop {
            match Database::connect(opt.clone()).await {
                Ok(db) => {
                    // Verify connection with a simple query
                    let test =
                        Statement::from_string(DatabaseBackend::Postgres, "SELECT 1".to_owned());

                    match db.execute(test).await {
                        Ok(_) => return Ok(db),
                        Err(e) if retries > 0 => {
                            eprintln!(
                                "Database connected but test query failed (retries left: {}): {}",
                                retries, e
                            );
                            // Fall through to retry logic below
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Database connected but not responsive: {}",
                                e
                            ));
                        }
                    }
                }
                Err(e) if retries > 0 => {
                    eprintln!(
                        "Failed to connect to database (retries left: {}): {}",
                        retries, e
                    );
                    // Fall through to retry logic below
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to connect to database: {}", e));
                }
            }

            if retries > 0 {
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await; // Reduced from 3 to 1 second
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to connect to database after {} retries",
                    max_retries
                ));
            }
        }
    }

    /// Connect to database with schema-specific search_path
    async fn connect_with_retry_schema(
        database_url: &str,
        schema_name: &str,
        max_retries: u32,
    ) -> anyhow::Result<DbConnection> {
        use sea_orm::ConnectOptions;
        use std::time::Duration;

        let mut retries = max_retries;

        // Create connection options with schema in search_path
        let mut opt = ConnectOptions::new(database_url.to_owned());
        opt.max_connections(5)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(10))
            .max_lifetime(Duration::from_secs(60))
            .sqlx_logging(false);

        loop {
            match Database::connect(opt.clone()).await {
                Ok(db) => {
                    // Verify connection and search_path (include public for TimescaleDB)
                    let test = Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!("SET search_path TO {}, public", schema_name),
                    );

                    match db.execute(test).await {
                        Ok(_) => {
                            // Verify search_path is set correctly
                            let check = Statement::from_string(
                                DatabaseBackend::Postgres,
                                "SHOW search_path".to_owned(),
                            );
                            match db.query_one(check).await {
                                Ok(_) => return Ok(db),
                                Err(e) if retries > 0 => {
                                    eprintln!(
                                        "Search path verification failed (retries left: {}): {}",
                                        retries, e
                                    );
                                }
                                Err(e) => {
                                    return Err(anyhow::anyhow!(
                                        "Failed to verify search_path: {}",
                                        e
                                    ));
                                }
                            }
                        }
                        Err(e) if retries > 0 => {
                            eprintln!(
                                "Failed to set search_path (retries left: {}): {}",
                                retries, e
                            );
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("Failed to set search_path: {}", e));
                        }
                    }
                }
                Err(e) if retries > 0 => {
                    eprintln!(
                        "Failed to connect to database (retries left: {}): {}",
                        retries, e
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to connect to database: {}", e));
                }
            }

            if retries > 0 {
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to connect to database after {} retries",
                    max_retries
                ));
            }
        }
    }

    /// Execute raw SQL query for testing
    pub async fn execute_sql(&self, sql: &str) -> anyhow::Result<ExecResult> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
        let result = self
            .db
            .execute(statement)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Query raw SQL and return results
    pub async fn query_sql(&self, sql: &str) -> anyhow::Result<Vec<QueryResult>> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
        let result = self
            .db
            .query_all(statement)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Clean up all data in the database (useful for test cleanup)
    ///
    /// This truncates all tables except migration-related tables.
    /// Tables are truncated in reverse dependency order to avoid foreign key issues.
    pub async fn cleanup_all_tables(&self) -> anyhow::Result<()> {
        // First, drop all TimescaleDB continuous aggregates (materialized views)
        let views = self
            .query_sql("SELECT matviewname FROM pg_matviews WHERE schemaname = 'public'")
            .await?;

        for view in views {
            if let Ok(view_name) = view.try_get::<String>("", "matviewname") {
                let sql = format!("DROP MATERIALIZED VIEW IF EXISTS {} CASCADE", view_name);
                self.execute_sql(&sql).await.ok(); // Ignore errors
            }
        }

        // Drop all custom types (enums and composites created by TimescaleDB)
        let types = self
            .query_sql(
                "SELECT t.typname FROM pg_type t
                 JOIN pg_namespace n ON n.oid = t.typnamespace
                 WHERE n.nspname = 'public'
                 AND t.typtype IN ('e', 'c')
                 AND t.typname NOT LIKE 'pg_%'",
            )
            .await?;

        for type_row in types {
            if let Ok(type_name) = type_row.try_get::<String>("", "typname") {
                let sql = format!("DROP TYPE IF EXISTS {} CASCADE", type_name);
                self.execute_sql(&sql).await.ok(); // Ignore errors
            }
        }

        // Get all table names except migration tables
        let tables = self
            .query_sql(
                "SELECT tablename FROM pg_tables
             WHERE schemaname = 'public'
             AND tablename NOT IN ('seaql_migrations', '_sqlx_migrations')
             ORDER BY tablename DESC",
            )
            .await?;

        // Truncate each table
        for table in tables {
            if let Ok(table_name) = table.try_get::<String>("", "tablename") {
                let sql = format!("TRUNCATE TABLE {} CASCADE", table_name);
                self.execute_sql(&sql).await?;
            }
        }

        Ok(())
    }

    /// Test database connectivity
    pub async fn test_connection(&self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "SELECT 1".to_owned());
        let result = self.db.query_one(statement).await?;

        if result.is_none() {
            return Err(anyhow::anyhow!("Connection test failed"));
        }

        Ok(())
    }

    /// Enable TimescaleDB extension on a table
    ///
    /// This is useful for tables that need time-series functionality
    pub async fn create_hypertable(
        &self,
        table_name: &str,
        time_column: &str,
    ) -> anyhow::Result<()> {
        // First ensure TimescaleDB extension is created
        self.execute_sql("CREATE EXTENSION IF NOT EXISTS timescaledb")
            .await?;

        // Create hypertable
        let sql = format!(
            "SELECT create_hypertable('{}', '{}', if_not_exists => TRUE)",
            table_name, time_column
        );
        self.execute_sql(&sql).await?;

        Ok(())
    }

    /// Get the database connection
    pub fn connection(&self) -> &DbConnection {
        &self.db
    }

    /// Get the database connection as Arc
    pub fn connection_arc(&self) -> Arc<DbConnection> {
        Arc::clone(&self.db)
    }
}

/// Test transaction helper for rollback-based testing
///
/// This allows you to run tests in a transaction that gets rolled back,
/// keeping your test database clean.
///
/// Note: This is a simplified transaction helper. For more complex transaction
/// handling, consider using SeaORM's built-in transaction support.
pub struct TestTransaction {
    pub db: Arc<DbConnection>,
}

impl TestTransaction {
    /// Start a new test transaction
    pub async fn new(db: Arc<DbConnection>) -> anyhow::Result<Self> {
        // Start transaction
        let statement = Statement::from_string(DatabaseBackend::Postgres, "BEGIN".to_owned());
        db.execute(statement).await?;

        Ok(TestTransaction { db })
    }

    /// Rollback the transaction
    pub async fn rollback(self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "ROLLBACK".to_owned());
        self.db.execute(statement).await?;
        Ok(())
    }

    /// Commit the transaction (use sparingly in tests)
    pub async fn commit(self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "COMMIT".to_owned());
        self.db.execute(statement).await?;
        Ok(())
    }
}

/// Helper to create a unique test database name
pub fn generate_test_db_name(prefix: &str) -> String {
    format!(
        "{}_{}",
        prefix,
        uuid::Uuid::new_v4().to_string().replace("-", "")
    )
}

/// Helper to wait for a condition with timeout
pub async fn wait_for<F, Fut>(
    condition: F,
    timeout_secs: u64,
    check_interval_ms: u64,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let interval = std::time::Duration::from_millis(check_interval_ms);

    while start.elapsed() < timeout {
        if condition().await {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }

    Err(anyhow::anyhow!("Timeout waiting for condition"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_runtime_unavailable_recognises_missing_docker_runtime() {
        for message in [
            "failed to initialize a docker client: Socket not found: /var/run/docker.sock",
            "Failed to Initialize a Docker Client: client error",
            "Socket not found: /var/run/docker.sock",
            "error trying to connect: No such file or directory (os error 2) while opening /var/run/docker.sock",
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?",
            "could not find Docker environment",
            "hyper legacy client: client error (connect)",
        ] {
            assert!(
                is_container_runtime_unavailable(message),
                "expected {message:?} to be classified as runtime unavailable"
            );
        }
    }

    #[test]
    fn container_runtime_unavailable_does_not_swallow_real_failures() {
        for message in [
            "migration m20260101_000001 failed: relation \"projects\" already exists",
            "No such file or directory (os error 2): /tmp/fixtures/missing.sql",
            "Socket not found: /run/postgresql/.s.PGSQL.5432",
            "error returned from database: duplicate key value violates unique constraint",
            "pull access denied for timescale/does-not-exist",
        ] {
            assert!(
                !is_container_runtime_unavailable(message),
                "expected {message:?} NOT to be classified as runtime unavailable"
            );
        }
    }

    async fn test_database_or_skip() -> anyhow::Result<Option<TestDatabase>> {
        match TestDatabase::new().await {
            Ok(database) => Ok(Some(database)),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping Docker-dependent database test: {error}");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    #[test]
    fn container_runtime_error_classification_does_not_hide_test_failures() {
        assert!(is_container_runtime_unavailable(
            "Error in the hyper legacy client: client error (Connect)"
        ));
        assert!(is_container_runtime_unavailable(
            "Docker daemon is unavailable: connection refused"
        ));
        assert!(is_container_runtime_unavailable(
            "failed to initialize a docker client: Socket not found: /var/run/docker.sock"
        ));
        assert!(!is_container_runtime_unavailable(
            "Failed to run migrations: column does not exist"
        ));
        assert!(!is_container_runtime_unavailable(
            "Migrations did not create expected tables"
        ));
        assert!(!is_container_runtime_unavailable(
            "Failed to pull timescale/timescaledb-ha: manifest unknown"
        ));
    }

    #[tokio::test]
    async fn schema_cleanup_gives_up_when_a_lock_is_held() -> anyhow::Result<()> {
        use sea_orm::TransactionTrait;
        let Some(test_db) = test_database_or_skip().await? else {
            return Ok(());
        };
        let schema = test_db
            .schema_name
            .clone()
            .ok_or_else(|| anyhow::anyhow!("test database has no schema of its own"))?;
        test_db
            .execute_sql("CREATE TABLE held_lock_table (id INT)")
            .await?;

        // Another connection of the same test sits in a transaction holding a
        // lock that DROP SCHEMA ... CASCADE needs on the table.
        let holder = test_db.db.begin().await?;
        holder
            .execute_unprepared(&format!(
                "LOCK TABLE {schema}.held_lock_table IN ACCESS SHARE MODE"
            ))
            .await?;

        let started = std::time::Instant::now();
        let dropped = TestDatabase::drop_schema_bounded(
            test_db.db.as_ref(),
            &schema,
            std::time::Duration::from_millis(300),
        )
        .await;
        let error = dropped.expect_err("a held lock must stop the schema drop");
        assert!(
            error.to_string().contains("lock timeout"),
            "unexpected error: {error}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "the drop must be bounded by the lock timeout, took {:?}",
            started.elapsed()
        );

        // Once the holder is gone the same drop goes through.
        holder.rollback().await?;
        TestDatabase::drop_schema_bounded(
            test_db.db.as_ref(),
            &schema,
            std::time::Duration::from_secs(5),
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_database_setup() -> anyhow::Result<()> {
        let Some(test_db) = test_database_or_skip().await? else {
            return Ok(());
        };

        // Test basic connectivity
        test_db.test_connection().await?;

        // Test raw SQL execution
        let result = test_db.query_sql("SELECT 1 as test_value").await?;
        assert_eq!(result.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_transaction_rollback() -> anyhow::Result<()> {
        let Some(test_db) = test_database_or_skip().await? else {
            return Ok(());
        };

        // Create a test table
        test_db
            .execute_sql("CREATE TABLE test_table (id SERIAL PRIMARY KEY, value TEXT)")
            .await?;

        // Insert data without transaction first to test normal operation
        test_db
            .execute_sql("INSERT INTO test_table (value) VALUES ('persistent')")
            .await?;

        // Verify data exists
        let result = test_db.query_sql("SELECT * FROM test_table").await?;
        assert_eq!(result.len(), 1);

        // Now test transaction rollback using SeaORM's transaction support
        // This test demonstrates that the TestTransaction struct can be used
        // Note: For proper transaction testing, consider using SeaORM's
        // built-in transaction support instead of raw SQL BEGIN/ROLLBACK

        // Clean up
        test_db.execute_sql("DELETE FROM test_table").await?;

        // Verify cleanup
        let result = test_db.query_sql("SELECT * FROM test_table").await?;
        assert_eq!(result.len(), 0);

        Ok(())
    }

    /// Test that parallel tests using TestDatabase::new() get isolated schemas
    #[tokio::test]
    async fn test_concurrent_test_isolation() -> anyhow::Result<()> {
        use std::sync::Arc;
        use tokio::sync::Barrier;

        if test_database_or_skip().await?.is_none() {
            return Ok(());
        }

        // Create a barrier to ensure all tests start at the same time
        let barrier = Arc::new(Barrier::new(5));

        // Spawn 5 concurrent "tests" that all try to create and use the same table
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    // Wait for all tasks to be ready
                    barrier.wait().await;

                    // Each gets its own TestDatabase with unique schema
                    let test_db = TestDatabase::new().await.unwrap();

                    // All create a table with the same name (would conflict without isolation)
                    test_db
                        .execute_sql("CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT)")
                        .await
                        .unwrap();

                    // Insert test-specific data
                    let insert_sql = format!("INSERT INTO users (name) VALUES ('user_{}')", i);
                    test_db.execute_sql(&insert_sql).await.unwrap();

                    // Verify only our data exists (isolation working)
                    let result = test_db.query_sql("SELECT * FROM users").await.unwrap();
                    assert_eq!(result.len(), 1, "Test {} should only see its own data", i);

                    // Verify the correct data
                    let name: String = result[0].try_get("", "name").unwrap();
                    assert_eq!(name, format!("user_{}", i));

                    i
                })
            })
            .collect();

        // Wait for all tasks to complete
        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Verify all 5 tests completed successfully
        assert_eq!(results.len(), 5);
        assert_eq!(results, vec![0, 1, 2, 3, 4]);

        Ok(())
    }

    /// Test that each TestDatabase instance gets a unique schema
    #[tokio::test]
    async fn test_unique_schemas() -> anyhow::Result<()> {
        if test_database_or_skip().await?.is_none() {
            return Ok(());
        }

        let db1 = TestDatabase::new().await?;
        let db2 = TestDatabase::new().await?;
        let db3 = TestDatabase::new().await?;

        // Get the current schema for each connection
        let schema1 = db1
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        let schema2 = db2
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        let schema3 = db3
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        // All schemas should be different
        assert_ne!(schema1, schema2);
        assert_ne!(schema2, schema3);
        assert_ne!(schema1, schema3);

        // All should start with "s" (our schema naming convention)
        assert!(schema1.starts_with('s'));
        assert!(schema2.starts_with('s'));
        assert!(schema3.starts_with('s'));

        Ok(())
    }

    /// Test that with_migrations works with unique schemas
    #[tokio::test]
    async fn test_with_migrations_concurrent() -> anyhow::Result<()> {
        use std::sync::Arc;
        use tokio::sync::Barrier;

        if test_database_or_skip().await?.is_none() {
            return Ok(());
        }

        let barrier = Arc::new(Barrier::new(3));

        let handles: Vec<_> = (0..3)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;

                    // Each gets its own database with migrations
                    let test_db = TestDatabase::with_migrations().await.unwrap();

                    // Verify users table exists (from migrations)
                    let result = test_db
                        .query_sql("SELECT column_name FROM information_schema.columns WHERE table_name = 'users'")
                        .await
                        .unwrap();

                    assert!(!result.is_empty(), "Test {} should have users table", i);
                    i
                })
            })
            .collect();

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(results, vec![0, 1, 2]);

        Ok(())
    }

    #[test]
    fn with_database_name_replaces_only_the_database() -> anyhow::Result<()> {
        assert_eq!(
            with_database_name("postgresql://u:p@localhost:5432/test_db", "temps_test_x")?,
            "postgresql://u:p@localhost:5432/temps_test_x"
        );
        assert_eq!(
            with_database_name(
                "postgres://u:p@db.internal/app?sslmode=require",
                "temps_tpl_y"
            )?,
            "postgres://u:p@db.internal/temps_tpl_y?sslmode=require"
        );
        // No database in the URL at all.
        assert_eq!(
            with_database_name("postgresql://u@localhost:5432", "temps_test_z")?,
            "postgresql://u@localhost:5432/temps_test_z"
        );
        assert!(with_database_name("localhost:5432/test_db", "x").is_err());
        Ok(())
    }

    #[test]
    fn cloned_database_url_accepts_appended_connection_options() -> anyhow::Result<()> {
        assert_eq!(
            cloned_database_url("postgresql://u:p@localhost:5432/test_db", "temps_test_x")?,
            "postgresql://u:p@localhost:5432/temps_test_x?options=-c search_path=public"
        );
        assert_eq!(
            cloned_database_url(
                "postgres://u:p@db.internal/app?sslmode=require",
                "temps_test_y"
            )?,
            "postgres://u:p@db.internal/temps_test_y?sslmode=require&options=-c search_path=public"
        );
        // What tests that tune their own pool do: the setting must join the
        // options parameter, not the database name.
        let tuned = format!(
            "{} -c deadlock_timeout=50ms",
            cloned_database_url("postgresql://u@localhost/test_db", "temps_test_z")?
        );
        assert!(
            tuned.contains("/temps_test_z?options=-c search_path=public -c deadlock_timeout=50ms")
        );
        Ok(())
    }

    #[test]
    fn template_name_is_a_valid_identifier_and_scoped_to_the_run() {
        let first = template_database_name::<Migrator>("run-a");
        assert_eq!(first, template_database_name::<Migrator>("run-a"));
        assert_ne!(first, template_database_name::<Migrator>("run-b"));
        assert!(first.starts_with(TEMPLATE_DATABASE_PREFIX));
        // Unquoted Postgres identifier: lowercase, digits, underscores, <= 63.
        assert!(first.len() <= 63);
        assert!(first
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
    }

    #[test]
    fn create_database_denial_is_told_apart_from_real_failures() {
        assert!(is_create_database_denied(
            "error returned from database: permission denied to create database"
        ));
        assert!(!is_create_database_denied(
            "error returned from database: database \"temps_tpl_1\" already exists"
        ));
        assert!(!is_create_database_denied(
            "source database \"temps_tpl_1\" is being accessed by other users"
        ));
    }

    async fn migrated_database_or_skip() -> anyhow::Result<Option<TestDatabase>> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Ok(Some(database)),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping Docker-dependent database test: {error}");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Two migrated test databases share nothing, and each is dropped again
    /// when its `TestDatabase` goes away.
    #[tokio::test]
    async fn migrated_databases_are_isolated_and_dropped() -> anyhow::Result<()> {
        let Some(first) = migrated_database_or_skip().await? else {
            return Ok(());
        };
        let second = TestDatabase::with_migrations().await?;

        let Some(ref cloned) = first.database_name else {
            eprintln!("Server refused CREATE DATABASE; schema fallback in use, nothing to check");
            return Ok(());
        };
        let first_name = cloned.name.clone();
        let server_url = cloned.server_url.clone();
        assert!(first_name.starts_with(TEST_DATABASE_PREFIX));
        assert_ne!(
            Some(first_name.as_str()),
            second.database_name.as_ref().map(|d| d.name.as_str()),
            "every test must get a database of its own"
        );

        first
            .execute_sql("CREATE TABLE only_in_first (id INT)")
            .await?;
        let leaked = second
            .query_sql("SELECT 1 FROM pg_tables WHERE tablename = 'only_in_first'")
            .await?;
        assert!(
            leaked.is_empty(),
            "a table created in one test database leaked into another"
        );

        drop(first);
        let admin = connect_maintenance(&server_url).await?;
        let remaining = admin
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT 1 FROM pg_database WHERE datname = $1",
                [first_name.clone().into()],
            ))
            .await?;
        let _ = admin.close().await;
        assert!(
            remaining.is_empty(),
            "test database {first_name} was not dropped"
        );
        Ok(())
    }

    /// Test connecting to an external database via new_from_url
    #[tokio::test]
    async fn test_new_from_url() -> anyhow::Result<()> {
        // This test requires a running PostgreSQL database
        // Skip if TEMPS_TEST_DATABASE_URL is not set
        let database_url = match std::env::var("TEMPS_TEST_DATABASE_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("Skipping test_new_from_url: TEMPS_TEST_DATABASE_URL not set");
                return Ok(());
            }
        };

        // Create test database from external URL
        let test_db = TestDatabase::new_from_url(&database_url).await?;

        // Verify connection works
        test_db.test_connection().await?;

        // Verify we have a unique schema
        let schema = test_db
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        assert!(schema.starts_with('s'), "Schema should start with 's'");

        // Verify we can create tables in our schema
        test_db
            .execute_sql("CREATE TABLE test_table (id SERIAL PRIMARY KEY, value TEXT)")
            .await?;

        // Insert and query data
        test_db
            .execute_sql("INSERT INTO test_table (value) VALUES ('test_value')")
            .await?;

        let result = test_db.query_sql("SELECT * FROM test_table").await?;
        assert_eq!(result.len(), 1);

        // Schema should be automatically cleaned up when test_db is dropped

        Ok(())
    }

    /// Test that TEMPS_TEST_DATABASE_URL environment variable is respected
    #[tokio::test]
    async fn test_env_var_connection() -> anyhow::Result<()> {
        // Skip if TEMPS_TEST_DATABASE_URL is not set
        if std::env::var("TEMPS_TEST_DATABASE_URL").is_err() {
            eprintln!("Skipping test_env_var_connection: TEMPS_TEST_DATABASE_URL not set");
            return Ok(());
        }

        // TestDatabase::new() should automatically use the env var
        let test_db = TestDatabase::new().await?;

        // Verify connection works
        test_db.test_connection().await?;

        // Verify we're not using a container (dedicated_container should be None)
        assert!(
            test_db.dedicated_container.is_none(),
            "Should not have a dedicated container when using env var"
        );

        // Verify we have a unique schema
        let schema = test_db
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        assert!(schema.starts_with('s'), "Schema should start with 's'");

        Ok(())
    }
}
