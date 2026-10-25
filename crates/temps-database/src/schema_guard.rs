// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup schema compatibility guard.
//!
//! Before any migration runs, compare the migrations recorded in
//! `seaql_migrations` with the ones compiled into this binary:
//!
//! - nothing recorded (or no ledger table at all): a fresh install;
//! - every recorded migration is known and none is pending: a plain restart;
//! - every recorded migration is known and some are pending: an upgrade of an
//!   existing installation, which is when a pre-migration backup is taken;
//! - a recorded migration this binary does not define: the database was
//!   migrated by a NEWER release, typically because only the binary was rolled
//!   back after an upgrade misbehaved. That is a supported recovery path, so
//!   the guard does not refuse it: it reports
//!   [`SchemaStatus::NewerThanBinary`] and logs a WARN naming the unknown
//!   migrations and the two ways back to a matching schema. The unknown
//!   migrations are left applied — an older binary has no `down()` for them —
//!   and only migrations this binary defines and the database lacks are
//!   applied (see `run_migrations`, which bypasses Sea-ORM's `Migrator::up`
//!   in this state because it refuses ledgers with unknown entries).
//!
//! The check is read-only: unlike `Migrator::get_pending_migrations`, it never
//! creates the `seaql_migrations` table, so a fresh database is left exactly as
//! it was found. It costs two small catalog/ledger queries, so it runs on every
//! start.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::MigratorTrait;
use temps_migrations::Migrator;
use tracing::warn;

use crate::DbConnection;

/// Version of this binary, as reported in guard warnings.
const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many unknown migration names a warning lists before summarising.
const MAX_LISTED_UNKNOWN: usize = 10;

/// Upgrade guide anchor referenced by guard warnings.
pub const UPGRADE_ROLLBACK_DOCS_URL: &str = "https://temps.sh/docs/upgrade#roll-back-an-upgrade";

/// Set once the newer-schema warning has been logged. Startup consults the
/// guard several times (backup preparation, the pending-migration probe, the
/// migration run itself); one warning per process is enough.
static NEWER_SCHEMA_WARNED: AtomicBool = AtomicBool::new(false);

/// Where the database schema stands relative to this binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStatus {
    /// No migration has ever been applied: a fresh install.
    Fresh,
    /// Every migration this binary defines is applied; nothing to do.
    UpToDate {
        /// Number of applied migrations.
        applied: usize,
    },
    /// An existing database with migrations this binary will apply.
    PendingUpgrade {
        /// Number of migrations already applied.
        applied: usize,
        /// Pending migration names, in apply order.
        pending: Vec<String>,
    },
    /// The database has applied migrations this binary does not define: a
    /// newer release migrated it. Startup continues with a warning.
    NewerThanBinary {
        /// Number of migrations already applied (known and unknown).
        applied: usize,
        /// Applied migrations this binary does not define, sorted.
        unknown: Vec<String>,
        /// Migrations this binary defines that are not applied, in apply
        /// order. Empty for a plain binary rollback; non-empty only when the
        /// newer release lacked a migration this binary has (a divergent
        /// build).
        pending: Vec<String>,
    },
}

impl SchemaStatus {
    /// Migrations this binary will apply (empty for [`SchemaStatus::Fresh`],
    /// whose plan comes from the migrator, and [`SchemaStatus::UpToDate`]).
    pub fn pending(&self) -> &[String] {
        match self {
            SchemaStatus::PendingUpgrade { pending, .. }
            | SchemaStatus::NewerThanBinary { pending, .. } => pending,
            SchemaStatus::Fresh | SchemaStatus::UpToDate { .. } => &[],
        }
    }

    /// The operator-facing warning for a database migrated by a newer release,
    /// or `None` when the schema is not newer than this binary.
    pub fn newer_schema_warning(&self) -> Option<String> {
        match self {
            SchemaStatus::NewerThanBinary { unknown, .. } => {
                Some(newer_schema_message(BINARY_VERSION, unknown))
            }
            SchemaStatus::Fresh
            | SchemaStatus::UpToDate { .. }
            | SchemaStatus::PendingUpgrade { .. } => None,
        }
    }
}

/// Why the schema guard could not classify the database.
#[derive(Debug, thiserror::Error)]
pub enum SchemaGuardError {
    /// The migration ledger could not be read.
    #[error("Startup schema check failed while {operation}: {source}")]
    ReadLedger {
        /// What the guard was doing when the query failed.
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

fn newer_schema_message(binary_version: &str, unknown: &[String]) -> String {
    format!(
        "The database schema is newer than this Temps binary (v{binary_version}): \
         {count} applied migration(s) are unknown to it: {listed}. \
         A newer Temps release migrated this database, and this older binary was \
         started against it afterwards (for example after rolling back only the \
         binary). Starting anyway: the unknown migrations are left in place and \
         only migrations this binary defines are applied, but features that rely \
         on the newer schema may misbehave until the schema and binary match again. \
         To get back to a matching pair, either run the Temps release that applied \
         these migrations (or a newer one), or restore the database from the backup \
         taken before that upgrade (automatic pre-migration backups are kept in \
         <data dir>/backups/pre-migration) and then start this binary. See {docs}",
        count = unknown.len(),
        listed = list_names(unknown),
        docs = UPGRADE_ROLLBACK_DOCS_URL
    )
}

fn list_names(names: &[String]) -> String {
    if names.len() <= MAX_LISTED_UNKNOWN {
        return names.join(", ");
    }
    format!(
        "{}, ... and {} more",
        names[..MAX_LISTED_UNKNOWN].join(", "),
        names.len() - MAX_LISTED_UNKNOWN
    )
}

/// Classify the applied migrations against the ones this binary knows.
///
/// `known` must be in apply order; the pending list preserves it.
pub fn classify_schema(applied: &[String], known: &[String]) -> SchemaStatus {
    let known_set: HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut unknown: Vec<String> = applied
        .iter()
        .filter(|name| !known_set.contains(name.as_str()))
        .cloned()
        .collect();

    if applied.is_empty() {
        return SchemaStatus::Fresh;
    }

    let applied_set: HashSet<&str> = applied.iter().map(String::as_str).collect();
    let pending: Vec<String> = known
        .iter()
        .filter(|name| !applied_set.contains(name.as_str()))
        .cloned()
        .collect();
    let applied = applied_set.len();

    if !unknown.is_empty() {
        unknown.sort();
        unknown.dedup();
        return SchemaStatus::NewerThanBinary {
            applied,
            unknown,
            pending,
        };
    }
    if pending.is_empty() {
        SchemaStatus::UpToDate { applied }
    } else {
        SchemaStatus::PendingUpgrade { applied, pending }
    }
}

/// Names of every migration compiled into this binary, in apply order.
pub fn known_migration_names() -> Vec<String> {
    Migrator::migrations()
        .iter()
        .map(|migration| migration.name().to_string())
        .collect()
}

/// Read the migration ledger WITHOUT creating it and classify the database.
///
/// A database migrated by a newer release is reported as
/// [`SchemaStatus::NewerThanBinary`] and logged once per process at WARN; it
/// is not an error. Only a ledger that cannot be read fails. Callers must run
/// this before applying any migration or taking any other startup write.
pub async fn check_schema_compatibility(
    db: &DbConnection,
) -> Result<SchemaStatus, SchemaGuardError> {
    let applied = read_applied_migrations(db).await?;
    let status = classify_schema(&applied, &known_migration_names());
    if let SchemaStatus::NewerThanBinary {
        unknown, pending, ..
    } = &status
    {
        if !NEWER_SCHEMA_WARNED.swap(true, Ordering::Relaxed) {
            warn!(
                binary_version = BINARY_VERSION,
                unknown_migrations = ?unknown,
                pending_known_migrations = pending.len(),
                docs = UPGRADE_ROLLBACK_DOCS_URL,
                "{}",
                newer_schema_message(BINARY_VERSION, unknown)
            );
        }
    }
    Ok(status)
}

async fn read_applied_migrations(db: &DbConnection) -> Result<Vec<String>, SchemaGuardError> {
    let ledger_present = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT to_regclass('seaql_migrations') IS NOT NULL AS present".to_owned(),
        ))
        .await
        .map_err(|source| SchemaGuardError::ReadLedger {
            operation: "checking whether the seaql_migrations table exists",
            source,
        })?
        .map(|row| row.try_get::<bool>("", "present"))
        .transpose()
        .map_err(|source| SchemaGuardError::ReadLedger {
            operation: "decoding whether the seaql_migrations table exists",
            source,
        })?
        .unwrap_or(false);
    if !ledger_present {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT version FROM seaql_migrations".to_owned(),
        ))
        .await
        .map_err(|source| SchemaGuardError::ReadLedger {
            operation: "reading applied migrations from seaql_migrations",
            source,
        })?;
    rows.iter()
        .map(|row| {
            row.try_get::<String>("", "version")
                .map_err(|source| SchemaGuardError::ReadLedger {
                    operation: "decoding a seaql_migrations.version value",
                    source,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn empty_ledger_is_a_fresh_install() {
        let status = classify_schema(&[], &names(&["m1", "m2"]));
        assert_eq!(status, SchemaStatus::Fresh);
        assert!(status.pending().is_empty());
    }

    #[test]
    fn fully_applied_ledger_is_up_to_date() {
        let status = classify_schema(&names(&["m2", "m1"]), &names(&["m1", "m2"]));
        assert_eq!(status, SchemaStatus::UpToDate { applied: 2 });
    }

    #[test]
    fn partially_applied_ledger_is_a_pending_upgrade_in_apply_order() {
        let status = classify_schema(&names(&["m1"]), &names(&["m1", "m3", "m2"]));
        assert_eq!(
            status,
            SchemaStatus::PendingUpgrade {
                applied: 1,
                pending: names(&["m3", "m2"]),
            }
        );
        assert_eq!(status.pending(), names(&["m3", "m2"]).as_slice());
        assert!(status.newer_schema_warning().is_none());
    }

    #[test]
    fn unknown_applied_migration_is_a_warning_with_its_name_and_remediation() {
        let status = classify_schema(
            &names(&["m1", "m2", "m9_future_b", "m9_future_a"]),
            &names(&["m1", "m2"]),
        );
        assert_eq!(
            status,
            SchemaStatus::NewerThanBinary {
                applied: 4,
                unknown: names(&["m9_future_a", "m9_future_b"]),
                pending: Vec::new(),
            }
        );
        // A plain binary rollback has nothing of its own to apply.
        assert!(status.pending().is_empty());
        let message = status
            .newer_schema_warning()
            .expect("a newer database must produce a warning");
        assert!(message.contains("m9_future_a, m9_future_b"), "{message}");
        assert!(
            message.contains("newer than this Temps binary"),
            "{message}"
        );
        assert!(message.contains(BINARY_VERSION), "{message}");
        assert!(message.contains("Starting anyway"), "{message}");
        assert!(!message.contains("Refusing"), "{message}");
        assert!(message.contains("backups/pre-migration"), "{message}");
        assert!(message.contains(UPGRADE_ROLLBACK_DOCS_URL), "{message}");
    }

    #[tokio::test]
    async fn unprotected_existing_upgrade_is_refused_without_ledger_changes() -> anyhow::Result<()>
    {
        use sea_orm::ConnectionTrait;
        let test_db = match crate::test_utils::TestDatabase::new().await {
            Ok(db) => db,
            Err(error)
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping schema upgrade database test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let db = test_db.db.as_ref();
        db.execute_unprepared(
            "CREATE TABLE seaql_migrations (version text PRIMARY KEY, applied_at bigint NOT NULL)",
        )
        .await
        .expect("ledger");
        let first = known_migration_names().remove(0);
        db.execute(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO seaql_migrations (version, applied_at) VALUES ($1, 0)",
            [first.into()],
        ))
        .await
        .expect("seed migration");
        let error = crate::ensure_no_unprotected_upgrade(db)
            .await
            .expect_err("protected migration required");
        assert!(error.to_string().contains("Run `temps migrate`"));
        let after = read_applied_migrations(db).await.expect("unchanged ledger");
        assert_eq!(after.len(), 1);
        Ok(())
    }

    #[test]
    fn newer_schema_still_reports_this_binarys_own_pending_migrations() {
        // A database migrated by a divergent build: it has one migration this
        // binary lacks and lacks one this binary has. The unknown one is
        // reported, and the binary's own missing migration stays pending so
        // startup can still apply it.
        let status = classify_schema(&names(&["m1", "other_branch"]), &names(&["m1", "m2"]));
        assert_eq!(
            status,
            SchemaStatus::NewerThanBinary {
                applied: 2,
                unknown: names(&["other_branch"]),
                pending: names(&["m2"]),
            }
        );
        assert_eq!(status.pending(), names(&["m2"]).as_slice());
    }

    #[test]
    fn long_unknown_lists_are_summarised() {
        let applied: Vec<String> = (0..15).map(|i| format!("m_future_{i:02}")).collect();
        let message = classify_schema(&applied, &[])
            .newer_schema_warning()
            .expect("all unknown");
        assert!(message.contains("15 applied migration(s)"), "{message}");
        assert!(message.contains("... and 5 more"), "{message}");
        assert!(!message.contains("m_future_14"), "{message}");
    }

    #[test]
    fn known_names_match_the_compiled_migrator() {
        let known = known_migration_names();
        assert_eq!(known.len(), crate::defined_migration_count());
        let unique: HashSet<&String> = known.iter().collect();
        assert_eq!(unique.len(), known.len(), "migration names must be unique");
    }

    /// Against a real PostgreSQL: the guard must not create the ledger table,
    /// must classify each ledger shape, and must report a newer database as a
    /// status (not an error).
    /// Skips when no container runtime or test database is available.
    #[tokio::test]
    async fn guard_reads_the_real_ledger_without_creating_it() -> anyhow::Result<()> {
        let test_db = match crate::test_utils::TestDatabase::new().await {
            Ok(db) => db,
            Err(error)
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping schema guard database test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let db = test_db.db.as_ref();

        assert_eq!(check_schema_compatibility(db).await?, SchemaStatus::Fresh);
        let ledger = db
            .query_one(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT to_regclass('seaql_migrations') IS NOT NULL AS present".to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("no row"))?
            .try_get::<bool>("", "present")?;
        assert!(!ledger, "the guard must not create seaql_migrations");

        let known = known_migration_names();
        let first = known
            .first()
            .ok_or_else(|| anyhow::anyhow!("binary defines no migrations"))?;
        db.execute_unprepared(&format!(
            "CREATE TABLE seaql_migrations (version varchar PRIMARY KEY, applied_at bigint NOT NULL); \
             INSERT INTO seaql_migrations VALUES ('{first}', 0);"
        ))
        .await?;
        let status = check_schema_compatibility(db).await?;
        assert_eq!(
            status,
            SchemaStatus::PendingUpgrade {
                applied: 1,
                pending: known[1..].to_vec(),
            }
        );

        db.execute_unprepared(
            "INSERT INTO seaql_migrations VALUES ('m29991231_000001_from_the_future', 0);",
        )
        .await?;
        let status = check_schema_compatibility(db).await?;
        assert_eq!(
            status,
            SchemaStatus::NewerThanBinary {
                applied: 2,
                unknown: vec!["m29991231_000001_from_the_future".to_string()],
                pending: known[1..].to_vec(),
            }
        );
        Ok(())
    }
}
