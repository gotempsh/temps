// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Attribute audit events written before `m20261113_000013_vault_audit_attribution`
//! to their vaults, once.
//!
//! Each batch reads the next audit rows by primary key, up to the last id that
//! existed when the migration installed its trigger, and attributes them with
//! the same SQL function the trigger uses. The cursor commits with the batch,
//! so an interrupted run resumes where it stopped. When the range is exhausted
//! the cursor row is deleted; readers switch to the attribution table only
//! then. Audit rows are read, never written.
//!
//! A batch holds SHARE locks on `access_service_audit_scopes` and
//! `access_vault_service_history`: a membership row committed before the batch
//! is seen by it, and one written after it waits and then sees the batch's
//! keys. The locks are taken `NOWAIT`: a batch that meets a membership write in
//! progress fails at once and is retried by the caller, so a membership write
//! waits for at most one running batch and never deadlocks with one.
//!
//! It walks `audit_logs` by primary key only, so it does not wait for the
//! post-migration expression indexes and can run alongside their build.

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};

/// Audit rows per committed batch: one batch is one short transaction.
const BATCH_SIZE: i64 = 5_000;

/// What one run did, for the caller's log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VaultAuditBackfillSummary {
    /// Batches committed by this run.
    pub batches: u64,
    /// Vault attributions written by this run.
    pub attributed: i64,
    /// Whether every pre-migration event is now attributed.
    pub complete: bool,
}

/// Attribute pre-migration audit events to their vaults, resuming a previous
/// run. Returns once none is left; a no-op when already complete.
pub async fn backfill_vault_audit_attribution(
    db: &DatabaseConnection,
) -> Result<VaultAuditBackfillSummary, DbErr> {
    let mut summary = VaultAuditBackfillSummary::default();
    if !backfill_table_present(db).await? {
        summary.complete = true;
        return Ok(summary);
    }
    loop {
        match backfill_batch(db, BATCH_SIZE).await? {
            Some(attributed) => {
                summary.batches += 1;
                summary.attributed += attributed;
            }
            None => {
                summary.complete = true;
                return Ok(summary);
            }
        }
    }
}

async fn backfill_table_present(db: &DatabaseConnection) -> Result<bool, DbErr> {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT to_regclass(quote_ident(current_schema()) || '.access_vault_audit_backfill') IS NOT NULL AS present",
        ))
        .await?
        .ok_or_else(|| {
            DbErr::Custom("Cannot check the vault audit attribution backfill table".into())
        })?;
    row.try_get::<bool>("", "present")
}

/// One committed batch. `Some(attributions written)` while rows remain,
/// `None` once the range is exhausted and the cursor is gone.
async fn backfill_batch(db: &DatabaseConnection, batch_size: i64) -> Result<Option<i64>, DbErr> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='60s'")
        .await?;
    let cursor = tx
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT cursor_id, upper_id FROM access_vault_audit_backfill WHERE singleton FOR UPDATE",
        ))
        .await?;
    let Some(cursor) = cursor else {
        tx.commit().await?;
        return Ok(None);
    };
    tx.execute_unprepared(
        "LOCK TABLE access_service_audit_scopes, access_vault_service_history IN SHARE MODE NOWAIT",
    )
    .await?;
    let from: i32 = cursor.try_get("", "cursor_id")?;
    let upper: i32 = cursor.try_get("", "upper_id")?;
    let row = tx
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
WITH batch AS MATERIALIZED (
 SELECT id, audit_date, data FROM audit_logs
 WHERE id > $1 AND id <= $2 ORDER BY id LIMIT $3
), applied AS MATERIALIZED (
 SELECT coalesce(sum(access_vault_audit_attribute(id, audit_date, temps_audit_json(data), false)), 0)::bigint AS attributed
 FROM batch
)
SELECT (SELECT max(id) FROM batch) AS last_id, (SELECT attributed FROM applied) AS attributed
"#,
            [from.into(), upper.into(), batch_size.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("Vault audit attribution batch after audit id {from} returned no cursor")))?;
    let last: Option<i32> = row.try_get("", "last_id")?;
    let attributed: i64 = row.try_get("", "attributed")?;
    match last {
        Some(id) => {
            tx.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE access_vault_audit_backfill SET cursor_id=$1 WHERE singleton",
                [id.into()],
            ))
            .await?;
        }
        None => {
            tx.execute_unprepared("DELETE FROM access_vault_audit_backfill WHERE singleton")
                .await?;
        }
    }
    tx.commit().await?;
    Ok(last.map(|_| attributed))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap()
    }

    /// Batches commit, resume from the cursor without revisiting rows, stop at
    /// the migration's upper bound, and leave no cursor once done.
    #[tokio::test]
    async fn batches_resume_from_the_cursor_and_finish_once() {
        if !temps_test_utils::external_database_configured()
            && !temps_test_utils::docker_available().await
        {
            eprintln!("Skipping vault audit backfill batches: database unavailable");
            return;
        }
        let mut test = temps_test_utils::TestDatabase::with_migrations()
            .await
            .unwrap();
        let db = test.connection();
        // Pre-migration history: written without the trigger.
        db.execute_unprepared(
            r#"INSERT INTO access_vaults(id,name,service_ids) VALUES('vault_fixture','Fixture',ARRAY['svc_fixture']);
 ALTER TABLE audit_logs DISABLE TRIGGER access_vault_audit_on_log;
 INSERT INTO audit_logs(user_agent,operation_type,data,audit_date,created_at)
  SELECT 'fixture','UPDATE_ACCESS_SERVICE','{"resource_id":"svc_fixture"}',now()-n*interval '1 second',now() FROM generate_series(1,7) n;
 ALTER TABLE audit_logs ENABLE TRIGGER access_vault_audit_on_log;
 INSERT INTO access_vault_audit_backfill(upper_id) SELECT max(id) FROM audit_logs;
 INSERT INTO audit_logs(user_agent,operation_type,data,audit_date,created_at)
  VALUES('fixture','UPDATE_ACCESS_SERVICE','{"resource_id":"svc_fixture"}',now(),now());"#,
        )
        .await
        .unwrap();
        // The live event is the trigger's.
        assert_eq!(
            count(db, "SELECT count(*) AS n FROM access_vault_audit_events").await,
            1
        );
        // Three rows, then an interruption.
        assert_eq!(backfill_batch(db, 3).await.unwrap(), Some(3));
        assert_eq!(
            count(db, "SELECT count(*) AS n FROM access_vault_audit_events").await,
            4
        );
        let summary = backfill_vault_audit_attribution(db).await.unwrap();
        assert!(summary.complete);
        assert_eq!(summary.attributed, 4);
        assert_eq!(
            count(db, "SELECT count(*) AS n FROM access_vault_audit_events").await,
            8
        );
        assert_eq!(
            count(db, "SELECT count(*) AS n FROM access_vault_audit_backfill").await,
            0
        );
        // Done stays done.
        let again = backfill_vault_audit_attribution(db).await.unwrap();
        assert_eq!(
            again,
            VaultAuditBackfillSummary {
                batches: 0,
                attributed: 0,
                complete: true
            }
        );
        test.cleanup().await;
    }
}
