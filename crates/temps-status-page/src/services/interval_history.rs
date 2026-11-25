// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The check interval each past check ran under.
//!
//! Uptime coverage lets a check vouch for the endpoint for a grace period
//! derived from the monitor's check interval. If that interval were always
//! read from the monitor row, changing it would rewrite the coverage of
//! windows that have already ended. Every write that changes a monitor's
//! interval records the interval it replaces in
//! `status_monitor_interval_changes`, and coverage credits each check with
//! the interval in effect when it ran.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Set};
use temps_entities::status_monitor_interval_changes;

use super::types::normalize_check_interval_seconds;

/// Record that `monitor_id` moves from `stored_interval` to `new_interval`
/// now. Returns whether a row was written: nothing is recorded when both
/// normalise to the same interval, because coverage is unchanged.
///
/// Call it in the transaction that writes the new interval, with the monitor
/// row locked, so the recorded previous interval is the one being replaced
/// and the change is recorded only if the update commits. The time is taken
/// here, under that lock, not when the request arrived: an edit that waited
/// for the lock behind another must not be stamped before it. Rows are also
/// read in `id` order, which follows the lock, so the order of changes holds
/// even if two timestamps compare out of order. The scheduler reads the new
/// interval once the transaction commits, a moment after this time.
pub(crate) async fn record_interval_change<C: ConnectionTrait>(
    conn: &C,
    monitor_id: i32,
    stored_interval: i32,
    new_interval: i32,
) -> Result<bool, DbErr> {
    let previous = normalize_check_interval_seconds(stored_interval);
    if previous == normalize_check_interval_seconds(new_interval) {
        return Ok(false);
    }
    // Without RETURNING, so the write is a single statement on every backend.
    status_monitor_interval_changes::Entity::insert(status_monitor_interval_changes::ActiveModel {
        monitor_id: Set(monitor_id),
        previous_interval_seconds: Set(previous),
        changed_at: Set(Utc::now()),
        ..Default::default()
    })
    .exec_without_returning(conn)
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn records_the_normalised_interval_being_replaced() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        // 10 s was stored before the floor existed; 30 s is what ran.
        let written = record_interval_change(&db, 5, 10, 3600)
            .await
            .expect("the change is recorded");

        assert!(written);
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("status_monitor_interval_changes"), "{log}");
        assert!(log.contains("Int(Some(30))"), "{log}");
        assert!(log.contains("Int(Some(5))"), "{log}");
    }

    #[tokio::test]
    async fn an_unchanged_effective_interval_writes_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        // 0 and 60 both run every 60 s; 10 and 30 both run every 30 s.
        assert!(!record_interval_change(&db, 5, 60, 60).await.expect("no-op"));
        assert!(!record_interval_change(&db, 5, 0, 60).await.expect("no-op"));
        assert!(!record_interval_change(&db, 5, 10, 30).await.expect("no-op"));

        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn a_database_failure_is_returned() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([DbErr::Custom("interval history unavailable".to_string())])
            .into_connection();

        let result = record_interval_change(&db, 5, 60, 120).await;

        assert!(matches!(result, Err(DbErr::Custom(_))));
    }
}
