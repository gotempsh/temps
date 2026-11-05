// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL's lock table against the TimescaleDB chunk count.
//!
//! A statement that changes a hypertable's definition locks every chunk until
//! its transaction ends. Dropping a foreign key is the worst case Temps runs:
//! TimescaleDB copies the constraint onto every chunk, and the drop locks each
//! chunk's relation, its copy of the constraint and that copy's four
//! referential triggers, [`LOCKS_PER_CHUNK`] locks per chunk (measured on
//! TimescaleDB 2.31 / PostgreSQL 18). The locks come out of one shared table
//! of `max_locks_per_transaction × (max_connections +
//! max_prepared_transactions)` slots, so on a large history the transaction
//! fails with SQLSTATE 53200, "out of shared memory".
//!
//! [`LockCapacity`] reads both sides so `temps doctor` and the pre-update
//! check can warn before a binary with such a migration is installed, and
//! [`LockSlotsExhausted`] is what a migration or post-migration step that ran
//! out returns instead of the bare PostgreSQL error.

use sea_orm::{DbErr, RuntimeErr};
use sqlx::{PgConnection, PgPool};

/// Locks one transaction takes per chunk when it drops a foreign key on a
/// hypertable: the chunk, its copy of the constraint and that copy's four
/// referential triggers.
pub const LOCKS_PER_CHUNK: i64 = 6;

/// `max_locks_per_transaction` is suggested in multiples of this.
const SUGGESTION_STEP: i64 = 64;

/// PostgreSQL's default `max_locks_per_transaction`.
const DEFAULT_MAX_LOCKS_PER_TRANSACTION: i64 = 64;

/// Chunks of one hypertable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypertableChunks {
    /// The hypertable's name, schema-qualified when it is outside the current
    /// schema.
    pub hypertable: String,
    pub chunks: i64,
}

/// The lock table's size and the chunk count it has to cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockCapacity {
    pub max_locks_per_transaction: i64,
    pub max_connections: i64,
    pub max_prepared_transactions: i64,
    /// Whether the TimescaleDB extension is installed. Without it there are no
    /// chunks.
    pub timescaledb: bool,
    /// Every hypertable with at least one chunk, most chunks first.
    pub hypertables: Vec<HypertableChunks>,
}

impl LockCapacity {
    /// Backends the lock table is sized for.
    fn lock_table_backends(&self) -> i64 {
        (self.max_connections + self.max_prepared_transactions).max(1)
    }

    /// Lock slots PostgreSQL documents for the shared lock table. Background
    /// workers add a few more, so this is a lower bound, which is the right
    /// side to err on for a warning.
    pub fn lock_slots(&self) -> i64 {
        self.max_locks_per_transaction
            .saturating_mul(self.lock_table_backends())
    }

    /// Chunks across every hypertable.
    pub fn total_chunks(&self) -> i64 {
        self.hypertables.iter().map(|table| table.chunks).sum()
    }

    /// The hypertable with the most chunks, if there is one.
    pub fn largest_hypertable(&self) -> Option<&HypertableChunks> {
        self.hypertables.first()
    }

    /// Chunks of `table`, matched by the name it was listed under; zero for a
    /// plain table.
    pub fn chunks_of(&self, table: &str) -> i64 {
        self.hypertables
            .iter()
            .find(|candidate| candidate.hypertable == table)
            .map_or(0, |candidate| candidate.chunks)
    }

    /// Locks a transaction needs to change the constraints of every hypertable
    /// at once, as a single migration may.
    pub fn locks_needed(&self) -> i64 {
        self.total_chunks().saturating_mul(LOCKS_PER_CHUNK)
    }

    /// Whether such a transaction would not fit in the lock table.
    pub fn at_risk(&self) -> bool {
        self.locks_needed() > self.lock_slots()
    }

    /// The `max_locks_per_transaction` to recommend: room for
    /// [`Self::locks_needed`] twice over, so ordinary traffic still fits next
    /// to it, rounded up to a multiple of 64 and never below the current
    /// value.
    pub fn suggested_max_locks_per_transaction(&self) -> i64 {
        let needed = self.locks_needed().saturating_mul(2);
        let per_backend = ceil_div(needed, self.lock_table_backends());
        round_up(per_backend, SUGGESTION_STEP)
            .max(self.max_locks_per_transaction)
            .max(DEFAULT_MAX_LOCKS_PER_TRANSACTION)
    }

    /// The suggestion after a transaction already ran out at the current
    /// value: never less than twice the current value.
    fn suggestion_after_exhaustion(&self) -> i64 {
        self.suggested_max_locks_per_transaction().max(round_up(
            self.max_locks_per_transaction.saturating_mul(2),
            SUGGESTION_STEP,
        ))
    }

    /// One line describing the lock table and the chunks it has to cover.
    pub fn describe(&self) -> String {
        let largest = match self.largest_hypertable() {
            Some(largest) => format!(", the most in {} ({})", largest.hypertable, largest.chunks),
            None => String::new(),
        };
        format!(
            "{total} TimescaleDB chunk(s){largest}; changing the constraints of every hypertable \
             in one transaction takes about {needed} locks ({LOCKS_PER_CHUNK} per chunk), and the \
             lock table holds about {slots} (max_locks_per_transaction {max_locks} x \
             (max_connections {connections} + max_prepared_transactions {prepared}))",
            total = self.total_chunks(),
            needed = self.locks_needed(),
            slots = self.lock_slots(),
            max_locks = self.max_locks_per_transaction,
            connections = self.max_connections,
            prepared = self.max_prepared_transactions,
        )
    }

    /// What the operator should do about [`Self::at_risk`].
    pub fn remedy(&self) -> String {
        raise_instructions(self.suggested_max_locks_per_transaction())
    }
}

fn ceil_div(value: i64, divisor: i64) -> i64 {
    if value <= 0 {
        0
    } else {
        (value + divisor - 1) / divisor
    }
}

fn round_up(value: i64, step: i64) -> i64 {
    ceil_div(value, step).saturating_mul(step)
}

fn raise_instructions(suggested: i64) -> String {
    format!(
        "Raise max_locks_per_transaction to at least {suggested} (in postgresql.conf, or \
         `ALTER SYSTEM SET max_locks_per_transaction = {suggested};`) and restart PostgreSQL: the \
         setting only takes effect after a restart, a reload is not enough."
    )
}

const SETTINGS_SQL: &str = "\
    SELECT current_setting('max_locks_per_transaction')::bigint AS max_locks, \
           current_setting('max_connections')::bigint AS max_connections, \
           current_setting('max_prepared_transactions')::bigint AS max_prepared, \
           EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') AS timescaledb";

/// Only run when TimescaleDB is installed: the view does not exist otherwise.
const CHUNKS_SQL: &str = "\
    SELECT CASE WHEN hypertable_schema = current_schema() THEN hypertable_name::text \
                ELSE format('%I.%I', hypertable_schema, hypertable_name) END AS hypertable, \
           count(*)::bigint AS chunks \
    FROM timescaledb_information.chunks \
    GROUP BY 1 \
    ORDER BY 2 DESC, 1";

/// Read the lock settings and the chunk count per hypertable on a pooled
/// connection.
pub async fn read_lock_capacity(pool: &PgPool) -> Result<LockCapacity, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    read_lock_capacity_on(&mut connection).await
}

/// [`read_lock_capacity`] on a connection the caller holds, so one holding the
/// only connection of a size-one pool (as `temps migrate` does) reads on it
/// rather than waiting for another.
pub async fn read_lock_capacity_on(
    connection: &mut PgConnection,
) -> Result<LockCapacity, sqlx::Error> {
    let (max_locks_per_transaction, max_connections, max_prepared_transactions, timescaledb): (
        i64,
        i64,
        i64,
        bool,
    ) = sqlx::query_as(SETTINGS_SQL)
        .fetch_one(&mut *connection)
        .await?;
    let hypertables = if timescaledb {
        sqlx::query_as::<_, (String, i64)>(CHUNKS_SQL)
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(hypertable, chunks)| HypertableChunks { hypertable, chunks })
            .collect()
    } else {
        Vec::new()
    };
    Ok(LockCapacity {
        max_locks_per_transaction,
        max_connections,
        max_prepared_transactions,
        timescaledb,
        hypertables,
    })
}

/// Whether PostgreSQL ran out of lock slots: SQLSTATE 53200 with the lock
/// table's hint. Plain memory exhaustion shares the SQLSTATE but not the hint,
/// and raising `max_locks_per_transaction` would not help it.
pub fn is_lock_exhaustion_sqlx(error: &sqlx::Error) -> bool {
    let Some(database_error) = error.as_database_error() else {
        return false;
    };
    if database_error.code().as_deref() != Some("53200") {
        return false;
    }
    match database_error.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
        Some(pg) => match pg.hint() {
            Some(hint) => hint.contains("max_locks_per_transaction"),
            None => pg.message().contains("out of shared memory"),
        },
        None => database_error.message().contains("out of shared memory"),
    }
}

/// [`is_lock_exhaustion_sqlx`] for an error that came through SeaORM.
pub fn is_lock_exhaustion(error: &DbErr) -> bool {
    match error {
        DbErr::Exec(RuntimeErr::SqlxError(error))
        | DbErr::Query(RuntimeErr::SqlxError(error))
        | DbErr::Conn(RuntimeErr::SqlxError(error)) => is_lock_exhaustion_sqlx(error),
        _ => false,
    }
}

/// What ran out of lock slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStep {
    /// A migration, by name.
    Migration(String),
    /// A step of post-migration maintenance, by name.
    PostMigration(&'static str),
}

impl std::fmt::Display for SchemaStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Migration(name) => write!(f, "Migration {name}"),
            Self::PostMigration(name) => write!(f, "Post-migration step {name}"),
        }
    }
}

/// The table whose chunks filled the lock table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedTable {
    /// The table the failing statement was changing.
    Changed(String),
    /// PostgreSQL does not say which table a statement was locking when it
    /// ran out, so for a migration this is the hypertable with the most
    /// chunks: the one whose locks grow fastest.
    LargestHypertable(String),
    /// No hypertable exists, or the chunk count could not be read.
    Unknown,
}

/// A migration or post-migration step ran out of PostgreSQL lock slots
/// (SQLSTATE 53200). Its transaction was rolled back; raising
/// `max_locks_per_transaction` and restarting PostgreSQL lets it run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSlotsExhausted {
    pub step: SchemaStep,
    pub table: LockedTable,
    /// The lock table and chunk count, read after the failure. `None` with
    /// `capacity_error` set when they could not be read.
    pub capacity: Option<LockCapacity>,
    pub capacity_error: Option<String>,
    /// The PostgreSQL error, as reported.
    pub reason: String,
}

impl LockSlotsExhausted {
    /// Describe a lock exhaustion of `step`, reading the capacity on
    /// `connection`. A failed read is reported in the message rather than
    /// replacing the error.
    pub async fn read(
        connection: &mut PgConnection,
        step: SchemaStep,
        changed_table: Option<&str>,
        reason: String,
    ) -> Self {
        let (capacity, capacity_error) = match read_lock_capacity_on(connection).await {
            Ok(capacity) => (Some(capacity), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self::new(step, changed_table, capacity, capacity_error, reason)
    }

    pub fn new(
        step: SchemaStep,
        changed_table: Option<&str>,
        capacity: Option<LockCapacity>,
        capacity_error: Option<String>,
        reason: String,
    ) -> Self {
        let table = match (changed_table, &capacity) {
            (Some(table), _) => LockedTable::Changed(table.to_string()),
            (None, Some(capacity)) => match capacity.largest_hypertable() {
                Some(largest) => LockedTable::LargestHypertable(largest.hypertable.clone()),
                None => LockedTable::Unknown,
            },
            (None, None) => LockedTable::Unknown,
        };
        Self {
            step,
            table,
            capacity,
            capacity_error,
            reason,
        }
    }

    /// The `max_locks_per_transaction` the message recommends, when the
    /// settings could be read.
    pub fn suggested_max_locks_per_transaction(&self) -> Option<i64> {
        self.capacity
            .as_ref()
            .map(LockCapacity::suggestion_after_exhaustion)
    }
}

impl std::fmt::Display for LockSlotsExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ran out of PostgreSQL lock slots (SQLSTATE 53200, out of shared memory)",
            self.step
        )?;
        let chunks_of = |table: &str| {
            self.capacity
                .as_ref()
                .map_or(0, |capacity| capacity.chunks_of(table))
        };
        match &self.table {
            LockedTable::Changed(table) => {
                write!(f, " while changing table {table}")?;
                let chunks = chunks_of(table);
                if chunks > 0 {
                    write!(f, " ({chunks} chunks)")?;
                }
            }
            LockedTable::LargestHypertable(table) => write!(
                f,
                "; PostgreSQL does not name the table, and the hypertable with the most chunks \
                 is {table} ({} chunks)",
                chunks_of(table)
            )?,
            LockedTable::Unknown => {}
        }
        write!(f, ". Its transaction was rolled back. ")?;
        match (&self.capacity, &self.capacity_error) {
            (Some(capacity), _) => write!(
                f,
                "This instance has {}. {} ",
                capacity.describe(),
                raise_instructions(capacity.suggestion_after_exhaustion())
            )?,
            (None, error) => write!(
                f,
                "The lock settings could not be read ({}). Raise max_locks_per_transaction to at \
                 least twice its current value (the default is 64) in postgresql.conf and restart \
                 PostgreSQL: the setting only takes effect after a restart, a reload is not \
                 enough. ",
                error.as_deref().unwrap_or("no detail")
            )?,
        }
        write!(
            f,
            "Then run `temps migrate` again. PostgreSQL reported: {}",
            self.reason
        )
    }
}

impl std::error::Error for LockSlotsExhausted {}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(max_locks: i64, connections: i64, chunks: &[(&str, i64)]) -> LockCapacity {
        LockCapacity {
            max_locks_per_transaction: max_locks,
            max_connections: connections,
            max_prepared_transactions: 0,
            timescaledb: true,
            hypertables: chunks
                .iter()
                .map(|(hypertable, chunks)| HypertableChunks {
                    hypertable: hypertable.to_string(),
                    chunks: *chunks,
                })
                .collect(),
        }
    }

    #[test]
    fn default_settings_hold_about_6400_locks() {
        let defaults = capacity(64, 100, &[]);
        assert_eq!(defaults.lock_slots(), 6_400);
        assert!(!defaults.at_risk());
        assert_eq!(defaults.suggested_max_locks_per_transaction(), 64);
    }

    #[test]
    fn a_year_of_daily_space_partitioned_chunks_is_at_risk() {
        // Two hypertables with four space partitions and a chunk per day.
        let year = capacity(64, 100, &[("events", 1_460), ("proxy_logs", 1_460)]);
        assert_eq!(year.total_chunks(), 2_920);
        assert_eq!(year.locks_needed(), 17_520);
        assert!(year.at_risk());
        // 17 520 x 2 / 100 backends = 351, rounded up to a multiple of 64.
        assert_eq!(year.suggested_max_locks_per_transaction(), 384);
        let suggested = capacity(384, 100, &[("events", 1_460), ("proxy_logs", 1_460)]);
        assert!(!suggested.at_risk());
    }

    #[test]
    fn largest_hypertable_and_chunks_of_a_table() {
        let tables = capacity(64, 100, &[("proxy_logs", 900), ("events", 300)]);
        assert_eq!(
            tables.largest_hypertable().map(|t| t.hypertable.as_str()),
            Some("proxy_logs")
        );
        assert_eq!(tables.chunks_of("events"), 300);
        assert_eq!(tables.chunks_of("request_logs"), 0);
    }

    #[test]
    fn suggestion_never_lowers_the_current_value() {
        let generous = capacity(1_024, 100, &[("events", 10)]);
        assert_eq!(generous.suggested_max_locks_per_transaction(), 1_024);
    }

    #[test]
    fn suggestion_after_exhaustion_at_least_doubles() {
        // Few chunks, yet the transaction ran out: other sessions held the
        // slots, so a value at or below the current one cannot be the answer.
        let small = capacity(64, 100, &[("events", 10)]);
        assert_eq!(small.suggestion_after_exhaustion(), 128);
    }

    #[test]
    fn exhaustion_message_names_step_table_value_and_restart() {
        let error = LockSlotsExhausted::new(
            SchemaStep::PostMigration("detach_telemetry_foreign_keys"),
            Some("events"),
            Some(capacity(64, 100, &[("events", 2_000)])),
            None,
            "out of shared memory".to_string(),
        );
        let message = error.to_string();
        assert!(
            message.contains("Post-migration step detach_telemetry_foreign_keys"),
            "{message}"
        );
        assert!(message.contains("table events (2000 chunks)"), "{message}");
        assert!(message.contains("SQLSTATE 53200"), "{message}");
        // 2 000 x 6 x 2 / 100 = 240, rounded up to 256.
        assert_eq!(error.suggested_max_locks_per_transaction(), Some(256));
        assert!(
            message.contains("max_locks_per_transaction to at least 256"),
            "{message}"
        );
        assert!(message.contains("restart PostgreSQL"), "{message}");
        assert!(
            message.contains("only takes effect after a restart"),
            "{message}"
        );
    }

    #[test]
    fn migration_exhaustion_names_the_largest_hypertable() {
        let error = LockSlotsExhausted::new(
            SchemaStep::Migration("m20261105_000001_project_data_purges".to_string()),
            None,
            Some(capacity(64, 100, &[("proxy_logs", 1_500), ("events", 700)])),
            None,
            "out of shared memory".to_string(),
        );
        assert_eq!(
            error.table,
            LockedTable::LargestHypertable("proxy_logs".to_string())
        );
        let message = error.to_string();
        assert!(
            message.contains("Migration m20261105_000001_project_data_purges"),
            "{message}"
        );
        assert!(
            message.contains("the hypertable with the most chunks is proxy_logs (1500 chunks)"),
            "{message}"
        );
    }

    #[test]
    fn unreadable_settings_still_explain_the_fix() {
        let error = LockSlotsExhausted::new(
            SchemaStep::Migration("m20261105_000001_project_data_purges".to_string()),
            None,
            None,
            Some("connection closed".to_string()),
            "out of shared memory".to_string(),
        );
        assert_eq!(error.table, LockedTable::Unknown);
        assert_eq!(error.suggested_max_locks_per_transaction(), None);
        let message = error.to_string();
        assert!(message.contains("connection closed"), "{message}");
        assert!(message.contains("restart"), "{message}");
    }

    #[test]
    fn non_database_errors_are_not_lock_exhaustion() {
        assert!(!is_lock_exhaustion(&DbErr::Custom(
            "out of shared memory".to_string()
        )));
        assert!(!is_lock_exhaustion_sqlx(&sqlx::Error::PoolTimedOut));
    }
}
