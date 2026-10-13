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
//!   migrated by a NEWER release. Starting an older binary against it would
//!   let code that does not understand the schema write to it, which is how a
//!   binary-only rollback corrupts data. The guard refuses with
//!   [`SchemaGuardError::DatabaseNewerThanBinary`], naming the unknown
//!   migrations and the two ways out.
//!
//! The check is read-only: unlike `Migrator::get_pending_migrations`, it never
//! creates the `seaql_migrations` table, so a refused start leaves the
//! database exactly as it found it. It costs two small catalog/ledger queries,
//! so it runs on every start.

use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::MigratorTrait;
use temps_migrations::Migrator;

use crate::DbConnection;

/// Version of this binary, as reported in guard errors.
const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many unknown migration names an error lists before summarising.
const MAX_LISTED_UNKNOWN: usize = 10;

/// Upgrade guide anchor referenced by guard errors.
pub const UPGRADE_ROLLBACK_DOCS_URL: &str = "https://temps.sh/docs/upgrade#roll-back-an-upgrade";

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
}

impl SchemaStatus {
    /// Pending migration names (empty unless [`SchemaStatus::PendingUpgrade`]).
    pub fn pending(&self) -> &[String] {
        match self {
            SchemaStatus::PendingUpgrade { pending, .. } => pending,
            SchemaStatus::Fresh | SchemaStatus::UpToDate { .. } => &[],
        }
    }
}

/// Why the schema guard refused to let migrations (and the server) proceed.
#[derive(Debug, thiserror::Error)]
pub enum SchemaGuardError {
    /// The database has applied migrations that this binary does not define.
    #[error(
        "The database schema is newer than this Temps binary (v{binary_version}): \
         {count} applied migration(s) are unknown to it: {listed}. \
         A newer Temps release migrated this database, and an older binary was \
         started against it afterwards (for example after rolling back only the \
         binary). Refusing to start so this binary cannot write to a schema it does \
         not understand; nothing was changed. To fix it, either run the Temps \
         release that applied these migrations (or a newer one), or restore the \
         database from the backup taken before that upgrade (automatic \
         pre-migration backups are kept in <data dir>/backups/pre-migration) and \
         then start this binary. See {docs}",
        count = unknown.len(),
        listed = list_names(unknown),
        docs = UPGRADE_ROLLBACK_DOCS_URL
    )]
    DatabaseNewerThanBinary {
        /// Version of the binary that refused to start.
        binary_version: &'static str,
        /// Applied migrations this binary does not define, sorted.
        unknown: Vec<String>,
    },

    /// The migration ledger could not be read.
    #[error("Startup schema check failed while {operation}: {source}")]
    ReadLedger {
        /// What the guard was doing when the query failed.
        operation: &'static str,
        #[source]
        source: DbErr,
    },
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
pub fn classify_schema(
    applied: &[String],
    known: &[String],
) -> Result<SchemaStatus, SchemaGuardError> {
    let known_set: HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut unknown: Vec<String> = applied
        .iter()
        .filter(|name| !known_set.contains(name.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        unknown.dedup();
        return Err(SchemaGuardError::DatabaseNewerThanBinary {
            binary_version: BINARY_VERSION,
            unknown,
        });
    }

    if applied.is_empty() {
        return Ok(SchemaStatus::Fresh);
    }

    let applied_set: HashSet<&str> = applied.iter().map(String::as_str).collect();
    let pending: Vec<String> = known
        .iter()
        .filter(|name| !applied_set.contains(name.as_str()))
        .cloned()
        .collect();
    let applied = applied_set.len();
    if pending.is_empty() {
        Ok(SchemaStatus::UpToDate { applied })
    } else {
        Ok(SchemaStatus::PendingUpgrade { applied, pending })
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
/// Returns [`SchemaGuardError::DatabaseNewerThanBinary`] when the database was
/// migrated by a newer release. Callers must run this before applying any
/// migration or taking any other startup write.
pub async fn check_schema_compatibility(
    db: &DbConnection,
) -> Result<SchemaStatus, SchemaGuardError> {
    let applied = read_applied_migrations(db).await?;
    classify_schema(&applied, &known_migration_names())
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
        let status = classify_schema(&[], &names(&["m1", "m2"])).expect("fresh");
        assert_eq!(status, SchemaStatus::Fresh);
        assert!(status.pending().is_empty());
    }

    #[test]
    fn fully_applied_ledger_is_up_to_date() {
        let status =
            classify_schema(&names(&["m2", "m1"]), &names(&["m1", "m2"])).expect("up to date");
        assert_eq!(status, SchemaStatus::UpToDate { applied: 2 });
    }

    #[test]
    fn partially_applied_ledger_is_a_pending_upgrade_in_apply_order() {
        let status =
            classify_schema(&names(&["m1"]), &names(&["m1", "m3", "m2"])).expect("pending upgrade");
        assert_eq!(
            status,
            SchemaStatus::PendingUpgrade {
                applied: 1,
                pending: names(&["m3", "m2"]),
            }
        );
        assert_eq!(status.pending(), names(&["m3", "m2"]).as_slice());
    }

    #[test]
    fn unknown_applied_migration_is_refused_with_its_name_and_remediation() {
        let error = classify_schema(
            &names(&["m1", "m9_future_b", "m9_future_a"]),
            &names(&["m1", "m2"]),
        )
        .expect_err("a newer database must be refused");
        match &error {
            SchemaGuardError::DatabaseNewerThanBinary {
                unknown,
                binary_version,
            } => {
                assert_eq!(unknown, &names(&["m9_future_a", "m9_future_b"]));
                assert_eq!(*binary_version, BINARY_VERSION);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("m9_future_a, m9_future_b"), "{message}");
        assert!(
            message.contains("newer than this Temps binary"),
            "{message}"
        );
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
    fn unknown_migrations_win_even_when_others_are_pending() {
        // A database migrated by a divergent build: it has one migration this
        // binary lacks and lacks one this binary has. Applying ours on top
        // would still mix two schemas, so it must be refused.
        let error = classify_schema(&names(&["m1", "other_branch"]), &names(&["m1", "m2"]))
            .expect_err("unknown applied migration must be refused");
        assert!(matches!(
            error,
            SchemaGuardError::DatabaseNewerThanBinary { ref unknown, .. }
                if unknown == &names(&["other_branch"])
        ));
    }

    #[test]
    fn long_unknown_lists_are_summarised() {
        let applied: Vec<String> = (0..15).map(|i| format!("m_future_{i:02}")).collect();
        let message = classify_schema(&applied, &[])
            .expect_err("all unknown")
            .to_string();
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
    /// must classify each ledger shape, and must refuse a newer database.
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
        let error = check_schema_compatibility(db)
            .await
            .expect_err("a newer database must be refused");
        assert!(matches!(
            error,
            SchemaGuardError::DatabaseNewerThanBinary { ref unknown, .. }
                if unknown == &vec!["m29991231_000001_from_the_future".to_string()]
        ));
        Ok(())
    }
}
