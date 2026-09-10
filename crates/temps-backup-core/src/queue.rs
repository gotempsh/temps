// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cancellation + schedule-completion SQL helpers shared by the executor and
//! `BackupService`.
//!
//! What's here:
//! - `cancel_backup` flips a `backups` row (and any stale `backup_jobs`
//!   sibling rows) to `failed` with a caller-provided reason.
//! - `cancel_schedule_run` cancels every non-terminal child of a
//!   `schedule_runs` row by iterating `cancel_backup`.
//! - `mark_schedule_run_finished_if_done` closes a `schedule_runs` row when
//!   all its children have reached terminal state.
//!
//! Historical note: this file once hosted the queue-based BackupRunner's
//! claim/lease/step-persist machinery. After the migration to the in-process
//! `BackupExecutor` the runner went away; these three helpers survive because
//! they operate on the `backups` and `schedule_runs` tables, which are
//! orthogonal to the runner's execution model.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value as SValue};
use thiserror::Error;

/// Errors returned by the SQL helpers in this module. Kept as a small,
/// queue-flavour-free enum now that the runner is gone.
#[derive(Debug, Error)]
pub enum BackupQueueError {
    #[error("Database error during {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Cancel one backup. Idempotent: returns `rows_affected == 0` if the backup
/// was already terminal. After flipping rows, calls
/// `mark_schedule_run_finished_if_done` so the parent `schedule_runs` row
/// closes when this was the last live child.
pub async fn cancel_backup(
    db: &DatabaseConnection,
    backup_id: i32,
    reason: &str,
) -> Result<u64, BackupQueueError> {
    use sea_orm::TransactionTrait;

    let txn = db.begin().await.map_err(|e| BackupQueueError::Database {
        operation: "cancel_backup:begin",
        source: e,
    })?;

    let backup_sql = r#"
UPDATE backups
   SET state         = 'failed',
       error_message = $1,
       finished_at   = COALESCE(finished_at, NOW())
 WHERE id            = $2
   AND state IN ('pending', 'running')
    "#;

    let result = txn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            backup_sql,
            vec![SValue::from(reason.to_owned()), SValue::from(backup_id)],
        ))
        .await
        .map_err(|e| BackupQueueError::Database {
            operation: "cancel_backup:update_backup",
            source: e,
        })?;

    txn.commit().await.map_err(|e| BackupQueueError::Database {
        operation: "cancel_backup:commit",
        source: e,
    })?;

    mark_schedule_run_finished_if_done(db, backup_id).await?;

    Ok(result.rows_affected())
}

/// Cancel every non-terminal backup belonging to a `schedule_runs` row.
/// Returns the total number of child backups flipped.
pub async fn cancel_schedule_run(
    db: &DatabaseConnection,
    schedule_run_id: i64,
    reason: &str,
) -> Result<u64, BackupQueueError> {
    use sea_orm::FromQueryResult;

    #[derive(FromQueryResult)]
    struct BackupId {
        id: i32,
    }

    let select_sql = r#"
SELECT id FROM backups
 WHERE schedule_run_id = $1
   AND state IN ('pending', 'running')
    "#;

    let rows = BackupId::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        select_sql,
        vec![SValue::from(schedule_run_id)],
    ))
    .all(db)
    .await
    .map_err(|e| BackupQueueError::Database {
        operation: "cancel_schedule_run:select",
        source: e,
    })?;

    let mut cancelled: u64 = 0;
    for row in rows {
        cancelled += cancel_backup(db, row.id, reason).await?;
    }
    Ok(cancelled)
}

/// Close the parent `schedule_runs` row when all children have reached a
/// terminal state. No-op when the backup has no `schedule_run_id`, when the
/// `schedule_runs` row is already finished, or when a sibling is still live.
pub async fn mark_schedule_run_finished_if_done(
    db: &DatabaseConnection,
    backup_id: i32,
) -> Result<(), BackupQueueError> {
    let sql = r#"
UPDATE schedule_runs sr
   SET finished_at = NOW()
 WHERE sr.id = (
     SELECT b.schedule_run_id
       FROM backups b
      WHERE b.id = $1
        AND b.schedule_run_id IS NOT NULL
   )
   AND sr.finished_at IS NULL
   AND NOT EXISTS (
       SELECT 1
         FROM backups b2
        WHERE b2.schedule_run_id = sr.id
          AND b2.state IN ('pending', 'running')
   )
    "#;

    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![SValue::from(backup_id)],
    ))
    .await
    .map_err(|e| BackupQueueError::Database {
        operation: "mark_schedule_run_finished_if_done",
        source: e,
    })?;

    Ok(())
}
