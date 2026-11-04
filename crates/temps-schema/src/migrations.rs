// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applying this binary's migrations, behind the schema guard.

use crate::schema_guard::{known_migration_names, SchemaStatus};
use sea_orm::sea_query::Query;
use sea_orm::{ConnectionTrait, DbErr, Statement, TransactionTrait};
use sea_orm_migration::{seaql_migrations, MigrationTrait, SchemaManager};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use temps_core::{ServiceError, ServiceResult};
use temps_database::DbConnection;
use temps_migrations::{Migrator, MigratorTrait};
use tokio::time::timeout;
use tracing::debug;

/// Overall timeout for running blocking migrations.
///
/// Raised from the original 120s because schema migrations on large
/// hypertables (e.g. an `ALTER` or backfill on a 20M+ row `proxy_logs`) can
/// legitimately take several minutes. This is the ceiling for migrations that
/// MUST complete before the proxy binds. Heavy, non-correctness-critical work
/// (large index builds) should NOT live in a blocking migration — see
/// `run_post_migration_indexes`, which builds them `CONCURRENTLY` after bind.
const MIGRATION_TIMEOUT: Duration = Duration::from_secs(600);

/// Per-statement lock acquisition timeout applied during migrations.
///
/// A blocking migration that cannot get its lock within this window fails fast
/// (and the service restarts to retry) rather than stalling the entire
/// `MIGRATION_TIMEOUT` budget waiting behind live traffic. This distinguishes
/// "stuck on a contended lock" (fail fast, retry) from "legitimately slow"
/// (holds its own lock, runs to completion).
const MIGRATION_LOCK_TIMEOUT_MS: u64 = 15_000;

/// Refuse an existing schema upgrade from a caller without backup preparation.
/// Fresh databases may still be initialized without a rollback snapshot.
pub async fn ensure_no_unprotected_upgrade(db: &DbConnection) -> ServiceResult<()> {
    let status = crate::check_schema_compatibility(db)
        .await
        .map_err(|error| ServiceError::Database(error.to_string()))?;
    if !status.pending().is_empty() {
        return Err(ServiceError::Database(format!(
            "Refusing {} pending migration(s) without a pre-migration backup. \
             Run `temps migrate` or `temps serve` with this binary first.",
            status.pending().len()
        )));
    }
    Ok(())
}

/// Open the connection pool and bring a fresh database up to this binary's
/// schema.
pub async fn establish_connection(database_url: &str) -> ServiceResult<Arc<DbConnection>> {
    let db = temps_database::connect_pooled(database_url).await?;

    // Utility commands and the standalone proxy cannot take a rollback backup.
    // Initialize fresh installs, but require the protected CLI upgrade path
    // before changing an existing database.
    ensure_no_unprotected_upgrade(&db).await?;
    run_migrations(&db).await?;

    // NOTE: continuous-aggregate backfill is intentionally NOT run here. It
    // requires a `CALL refresh_continuous_aggregate()` (a TimescaleDB operation
    // that can be slow) and is not needed to serve traffic. Callers that serve
    // requests (notably `temps serve`) spawn `run_post_migration_backfill` on a
    // long-lived runtime so it never delays startup / the proxy bind. The refresh
    // policy catches up regardless. See `run_post_migration_backfill`.

    Ok(Arc::new(db))
}

/// Run the schema guard, surfacing an unreadable ledger as a [`ServiceError`].
///
/// A database migrated by a newer release is NOT an error: the guard logs a
/// warning and returns [`SchemaStatus::NewerThanBinary`] so callers can keep
/// Sea-ORM's migrator (which rejects unknown ledger entries) out of the way.
async fn ensure_schema_compatible(db: &DbConnection) -> ServiceResult<SchemaStatus> {
    crate::schema_guard::check_schema_compatibility(db)
        .await
        .map_err(|error| match error {
            crate::schema_guard::SchemaGuardError::ReadLedger { .. } => {
                ServiceError::Database(error.to_string())
            }
        })
}

/// This binary's migrations named in `pending`, in apply order.
fn migrations_named(pending: &[String]) -> Vec<Box<dyn MigrationTrait>> {
    let wanted: HashSet<&str> = pending.iter().map(String::as_str).collect();
    Migrator::migrations()
        .into_iter()
        .filter(|migration| wanted.contains(migration.name()))
        .collect()
}

/// Apply one migration and record it in `seaql_migrations`, in a single
/// transaction — the same steps Sea-ORM's `Migrator::up` performs per
/// migration on PostgreSQL.
///
/// Used only when the ledger holds migrations this binary does not define
/// ([`SchemaStatus::NewerThanBinary`]): `Migrator::up` and
/// `Migrator::get_pending_migrations` fail outright on such a ledger
/// ("Migration file of version '…' is missing"), which would turn the guard's
/// warning back into a refused start. The unknown migrations are never rolled
/// back — this binary has no `down()` for them.
async fn apply_migration_on_newer_schema(
    db: &DbConnection,
    migration: &dyn MigrationTrait,
) -> Result<(), DbErr> {
    let applied_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            DbErr::Custom(format!(
                "Cannot record migration {}: system clock is before the UNIX epoch: {error}",
                migration.name()
            ))
        })?
        .as_secs() as i64;
    let record = Query::insert()
        .into_table(Migrator::migration_table_name())
        .columns([
            seaql_migrations::Column::Version,
            seaql_migrations::Column::AppliedAt,
        ])
        .values([migration.name().into(), applied_at.into()])
        .map_err(|error| {
            DbErr::Custom(format!(
                "Cannot build the ledger row for migration {}: {error}",
                migration.name()
            ))
        })?
        .to_owned();

    let txn = db.begin().await?;
    migration.up(&SchemaManager::new(&txn)).await?;
    txn.execute(txn.get_database_backend().build(&record))
        .await?;
    txn.commit().await
}

/// Apply all pending migrations.
///
/// Uses Sea-ORM's `Migrator::up`, which applies only migrations present in this
/// binary that are NOT yet recorded in `seaql_migrations`. Before that, the
/// schema guard ([`crate::schema_guard::check_schema_compatibility`]) checks
/// for applied migrations this binary does not define — i.e. a database
/// migrated by a newer release, typically after a binary-only rollback. That
/// is logged as a warning and startup continues: the unknown migrations are
/// left applied and only this binary's own missing migrations (normally none)
/// are applied, without `Migrator::up`, which would reject the ledger.
///
/// A short session `lock_timeout` is set first so a migration blocked on a
/// contended lock fails fast (and the operator retries) rather than burning the
/// entire `MIGRATION_TIMEOUT` budget waiting behind live traffic.
pub async fn run_migrations(db: &DbConnection) -> ServiceResult<()> {
    let status = ensure_schema_compatible(db).await?;

    // Fail fast on contended locks rather than hanging the whole budget.
    // Best-effort — non-fatal on setups that reject it.
    if let Err(e) = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SET lock_timeout = '{}ms'", MIGRATION_LOCK_TIMEOUT_MS),
        ))
        .await
    {
        debug!(
            "Could not set lock_timeout for migrations (non-fatal): {}",
            e
        );
    }

    if matches!(status, SchemaStatus::NewerThanBinary { .. }) {
        // Sea-ORM's `Migrator::up` rejects a ledger with unknown entries; the
        // streaming runner applies this binary's own missing migrations (if
        // any) without it.
        let report = run_migrations_streaming(db, |_| {}).await?;
        return match report.failed() {
            None => Ok(()),
            Some(failed) => Err(ServiceError::Database(format!(
                "Failed to apply migration {} on a database migrated by a newer Temps \
                 release: {}",
                failed.name,
                failed
                    .error
                    .as_deref()
                    .unwrap_or("no error detail reported")
            ))),
        };
    }

    match timeout(MIGRATION_TIMEOUT, Migrator::up(db, None)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ServiceError::Database(format!(
            "Failed to run migrations: {}",
            e
        ))),
        Err(_) => Err(ServiceError::Database(format!(
            "Database migrations timed out after {} seconds. For large databases, \
             run `temps migrate` manually with the new binary before restarting the server.",
            MIGRATION_TIMEOUT.as_secs()
        ))),
    }
}

/// Outcome of one migration applied by [`run_migrations_reported`].
#[derive(Debug, Clone)]
pub struct MigrationStepResult {
    /// The migration's name (e.g. `m20260603_000001_create_otel_trace_summaries`).
    pub name: String,
    /// Whether `up()` succeeded for this migration.
    pub success: bool,
    /// Wall-clock time the migration took.
    pub elapsed: Duration,
    /// The error message when `success` is false.
    pub error: Option<String>,
}

/// Report returned by [`run_migrations_reported`]: the plan (pending names
/// discovered before applying) and the per-migration results.
#[derive(Debug, Clone, Default)]
pub struct MigrationRunReport {
    /// Names of the migrations that were pending at the start, in apply order.
    pub planned: Vec<String>,
    /// One entry per migration actually attempted, in apply order. On the
    /// first failure, application stops and no further entries are added — so
    /// `results.len() <= planned.len()` and the last entry is the failure.
    pub results: Vec<MigrationStepResult>,
}

impl MigrationRunReport {
    /// True when every planned migration applied successfully (or there was
    /// nothing to apply).
    pub fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.success) && self.results.len() == self.planned.len()
    }

    /// The migration that failed, if any.
    pub fn failed(&self) -> Option<&MigrationStepResult> {
        self.results.iter().find(|r| !r.success)
    }
}

/// Progress event emitted by [`run_migrations_streaming`] as it works through
/// the pending list, so a CLI can give live feedback instead of waiting for the
/// whole run to finish.
#[derive(Debug, Clone)]
pub enum MigrationProgress<'a> {
    /// A migration is about to be applied. `index` is 1-based; `total` is the
    /// number of pending migrations. Emitted before `Migrator::up` runs so the
    /// operator sees which migration is currently in flight (the one that might
    /// hang on a slow index build).
    Started {
        index: usize,
        total: usize,
        name: &'a str,
    },
    /// A migration finished — successfully or not. Carries the same per-step
    /// result that ends up in [`MigrationRunReport::results`].
    Finished {
        index: usize,
        total: usize,
        result: &'a MigrationStepResult,
    },
}

/// Apply pending schema migrations one at a time, invoking `on_progress` before
/// and after each one so callers can give live feedback as the run proceeds.
///
/// This is the streaming core behind [`run_migrations_reported`]. It reads the
/// pending list via `Migrator::get_pending_migrations`, then applies each with
/// `Migrator::up(db, Some(1))`, timing each and stopping at the first failure.
/// `on_progress` fires with [`MigrationProgress::Started`] right before a
/// migration runs and [`MigrationProgress::Finished`] right after — so a CLI can
/// print "applying X…" / "✓ X (820ms)" lines as they happen instead of waiting
/// for the whole batch (a slow index build no longer looks like a hang).
///
/// The same `lock_timeout` guard is applied so a contended lock fails fast
/// instead of hanging, and each step is bounded by [`MIGRATION_TIMEOUT`]. This
/// does NOT run the post-migration continuous-aggregate backfill — the caller
/// runs [`run_post_migration_backfill`] afterwards.
pub async fn run_migrations_streaming<F>(
    db: &DbConnection,
    mut on_progress: F,
) -> ServiceResult<MigrationRunReport>
where
    F: FnMut(MigrationProgress<'_>),
{
    // Classify the ledger before touching anything. A database migrated by a
    // newer release is warned about and handled without `Migrator::up`.
    let status = ensure_schema_compatible(db).await?;

    // Fail fast on contended locks rather than hanging (mirrors run_migrations).
    if let Err(e) = db
        .execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SET lock_timeout = '{}ms'", MIGRATION_LOCK_TIMEOUT_MS),
        ))
        .await
    {
        debug!(
            "Could not set lock_timeout for migrations (non-fatal): {}",
            e
        );
    }

    // 1. Read the plan: which migrations are pending, in apply order.
    let newer_schema = matches!(status, SchemaStatus::NewerThanBinary { .. });
    let pending_names = if newer_schema {
        status.pending().to_vec()
    } else {
        Migrator::get_pending_migrations(db)
            .await
            .map_err(|e| {
                ServiceError::Database(format!("Failed to read pending migrations: {}", e))
            })?
            .iter()
            .map(|migration| migration.name().to_string())
            .collect()
    };
    // Only the newer-schema path applies migrations itself, so only it needs
    // the migration objects; `Migrator::up(db, Some(1))` finds its own.
    let newer_schema_migrations = if newer_schema {
        migrations_named(&pending_names)
    } else {
        Vec::new()
    };

    let total = pending_names.len();
    let mut report = MigrationRunReport {
        planned: pending_names.clone(),
        results: Vec::with_capacity(total),
    };

    // 2. Apply each pending migration individually so we can time and report it.
    //    `Migrator::up(db, Some(1))` applies exactly the next pending migration.
    for (i, name) in pending_names.into_iter().enumerate() {
        let index = i + 1;

        // Announce the migration BEFORE running it: this is the one that might
        // sit for a while on a large table, so the operator needs to see it now.
        on_progress(MigrationProgress::Started {
            index,
            total,
            name: &name,
        });

        let started = std::time::Instant::now();
        let apply = async {
            if !newer_schema {
                return Migrator::up(db, Some(1)).await;
            }
            match newer_schema_migrations
                .iter()
                .find(|migration| migration.name() == name)
            {
                Some(migration) => apply_migration_on_newer_schema(db, migration.as_ref()).await,
                None => Err(DbErr::Custom(format!(
                    "Migration {name} is not defined in this binary"
                ))),
            }
        };
        let outcome = match timeout(MIGRATION_TIMEOUT, apply).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{}", e)),
            Err(_) => Err(format!("timed out after {}s", MIGRATION_TIMEOUT.as_secs())),
        };
        let elapsed = started.elapsed();

        let (success, error) = match outcome {
            Ok(()) => (true, None),
            Err(err) => (false, Some(err)),
        };
        let result = MigrationStepResult {
            name,
            success,
            elapsed,
            error,
        };
        // Emit the finished result, then record it in the report.
        on_progress(MigrationProgress::Finished {
            index,
            total,
            result: &result,
        });
        report.results.push(result);

        // Stop at the first failure — later migrations may depend on the one
        // that just failed, and Sea-ORM won't have applied them.
        if !success {
            break;
        }
    }

    Ok(report)
}

/// Apply pending schema migrations one at a time, returning a [`MigrationRunReport`]
/// so callers can print a plan up front and a pass/fail line per migration.
///
/// Thin wrapper over [`run_migrations_streaming`] that discards progress events
/// and returns only the final report. Prefer `run_migrations_streaming` when you
/// want live per-migration feedback.
pub async fn run_migrations_reported(db: &DbConnection) -> ServiceResult<MigrationRunReport> {
    run_migrations_streaming(db, |_| {}).await
}

/// Read the list of pending migrations WITHOUT applying any of them.
///
/// Returns the names (in apply order) of migrations defined in this binary that
/// are not yet recorded in `seaql_migrations`. Used by `temps migrate --dry-run`
/// to show the plan and by the interactive `--confirm` gate to preview what
/// would run before the operator commits. This is read-only: it never mutates
/// the schema or the migration ledger, and never creates the ledger. A database
/// with no ledger has every migration pending.
///
/// Answered from the schema guard's read of the ledger rather than
/// `Migrator::get_pending_migrations`, which creates `seaql_migrations` when it
/// is missing.
pub async fn get_pending_migration_names(db: &DbConnection) -> ServiceResult<Vec<String>> {
    let status = ensure_schema_compatible(db).await?;
    Ok(match status {
        SchemaStatus::Fresh => known_migration_names(),
        other => other.pending().to_vec(),
    })
}

/// Number of migrations defined in this binary. Compared with the pending
/// count to tell a fresh install (everything pending) from an upgrade (some
/// pending on a database that already has applied migrations).
pub fn defined_migration_count() -> usize {
    Migrator::migrations().len()
}
