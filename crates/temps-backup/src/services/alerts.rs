// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backup alert watcher.
//!
//! Runs on a periodic task to detect two failure modes that are otherwise
//! invisible:
//!
//! 1. **Overdue schedules**: `enabled = true` AND `next_run < NOW() - 1h`.
//!    Scheduler did not enqueue when it should have.
//! 2. **Stalled jobs**: `state = 'pending'` AND `created_at < NOW() - 1h`.
//!    Runner did not claim the job.
//!
//! Auto-resolves alerts when the condition clears (schedule fires successfully,
//! job is claimed).

use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use tracing::{debug, info};

/// Grace period before an overdue schedule or stalled job triggers an alert.
/// Both conditions must persist for this long before the watcher opens an alert.
pub const OVERDUE_GRACE: Duration = Duration::from_secs(60 * 60);

/// Counts of alerts opened and resolved in a single sweep tick.
#[derive(Debug, Default, Clone, Copy)]
pub struct SweepStats {
    /// Number of `overdue_schedule` alerts opened this tick.
    pub opened_overdue: u64,
    /// Number of `stalled_job` alerts opened this tick.
    pub opened_stalled: u64,
    /// Number of `overdue_schedule` alerts resolved this tick.
    pub resolved_overdue: u64,
    /// Number of `stalled_job` alerts resolved this tick.
    pub resolved_stalled: u64,
}

impl SweepStats {
    /// Returns `true` if any alerts were opened or resolved this tick.
    pub fn has_changes(&self) -> bool {
        self.opened_overdue > 0
            || self.opened_stalled > 0
            || self.resolved_overdue > 0
            || self.resolved_stalled > 0
    }
}

/// A single tick of the backup alert watcher.
///
/// Performs four SQL statements in sequence:
///
/// **Step A** — Open `overdue_schedule` alerts for enabled schedules whose
/// `next_run` is more than 1 hour in the past and have no open alert yet.
/// Severity escalates to `critical` when the condition has persisted > 6 hours.
///
/// **Step B** — Open `stalled_job` alerts for `pending` jobs older than 1 hour
/// with no open alert yet.
///
/// **Step C** — Resolve `overdue_schedule` alerts whose condition has cleared
/// (schedule fired, was disabled, or `next_run` is now in the acceptable window).
///
/// **Step D** — Resolve `stalled_job` alerts whose job was claimed (state is no
/// longer `pending`).
///
/// The partial unique indexes `backup_alerts_one_open_per_schedule` and
/// `backup_alerts_one_open_per_job` make the `ON CONFLICT DO NOTHING` clauses in
/// Steps A and B safe to call repeatedly — idempotent.
///
/// Returns counts of alerts opened and resolved so the caller can decide log
/// verbosity.
pub async fn sweep_backup_alerts(db: &DatabaseConnection) -> Result<SweepStats, sea_orm::DbErr> {
    let mut stats = SweepStats::default();

    // ── Step A: Open overdue-schedule alerts ──────────────────────────────────
    //
    // Identifies enabled schedules whose next_run has passed by > 1 hour and
    // that have no open alert yet. Escalates to 'critical' when the condition
    // has persisted > 6 hours (scheduler has been dead for a long time).
    let sql_open_overdue = r#"
INSERT INTO backup_alerts (kind, schedule_id, severity, message, opened_at)
SELECT
    'overdue_schedule',
    s.id,
    CASE WHEN NOW() - s.next_run > INTERVAL '6 hours' THEN 'critical' ELSE 'warning' END,
    'Schedule "' || s.name || '" (id=' || s.id || ') has next_run ' || s.next_run || ' but no job has fired',
    NOW()
FROM backup_schedules s
WHERE s.enabled = true
  AND s.next_run IS NOT NULL
  AND s.next_run < NOW() - INTERVAL '1 hour'
  AND NOT EXISTS (
      SELECT 1 FROM backup_alerts a
      WHERE a.schedule_id = s.id AND a.resolved_at IS NULL
  )
ON CONFLICT DO NOTHING
RETURNING id
"#;

    let open_overdue_rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_open_overdue,
            vec![],
        ))
        .await?;

    let opened_overdue = open_overdue_rows.len() as u64;
    stats.opened_overdue = opened_overdue;

    for _ in &open_overdue_rows {
        // Log each new alert as INFO so operators see it without filtering.
        // We do not have the schedule_id/name in scope here; the message is in
        // the row. A single aggregate log below covers the batch.
    }

    if opened_overdue > 0 {
        info!(
            count = opened_overdue,
            kind = "overdue_schedule",
            "backup alert watcher: opened new overdue-schedule alerts"
        );
    }

    // ── Step B: Open stalled-backup alerts ────────────────────────────────────
    //
    // Identifies `backups` rows stuck in `pending` for >1 hour with no
    // open alert. After the queue migration this means: the trigger
    // inserted the row and published the message, but the consumer
    // never dispatched it (e.g. process crashed in the gap, or the
    // consumer is wedged).
    //
    // Since the FK column was dropped (job_id used to point at
    // backup_jobs), uniqueness is enforced by the message text alone —
    // good enough as a back-stop. The dedup is best-effort; the resolve
    // step below clears any stragglers.
    let sql_open_stalled = r#"
INSERT INTO backup_alerts (kind, severity, message, opened_at)
SELECT
    'stalled_job',
    'warning',
    'Backup ' || b.id || ' (backup_uuid=' || b.backup_id || ') has been pending for ' || (NOW() - b.started_at)::text,
    NOW()
FROM backups b
WHERE b.state = 'pending'
  AND b.started_at < NOW() - INTERVAL '1 hour'
  AND NOT EXISTS (
      SELECT 1 FROM backup_alerts a
      WHERE a.kind = 'stalled_job'
        AND a.resolved_at IS NULL
        AND a.message LIKE 'Backup ' || b.id || ' (%'
  )
RETURNING id
"#;

    let open_stalled_rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_open_stalled,
            vec![],
        ))
        .await?;

    let opened_stalled = open_stalled_rows.len() as u64;
    stats.opened_stalled = opened_stalled;

    if opened_stalled > 0 {
        info!(
            count = opened_stalled,
            kind = "stalled_job",
            "backup alert watcher: opened new stalled-job alerts"
        );
    }

    // ── Step C: Resolve overdue-schedule alerts whose condition cleared ────────
    //
    // Clears open overdue_schedule alerts when the schedule has fired (next_run
    // advanced past the threshold), was disabled, or next_run became NULL.
    let sql_resolve_overdue = r#"
UPDATE backup_alerts a
SET resolved_at = NOW()
FROM backup_schedules s
WHERE a.schedule_id = s.id
  AND a.resolved_at IS NULL
  AND a.kind = 'overdue_schedule'
  AND (
      s.next_run IS NULL
      OR s.next_run >= NOW() - INTERVAL '1 hour'
      OR s.enabled = false
  )
RETURNING a.id
"#;

    let resolve_overdue_rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_resolve_overdue,
            vec![],
        ))
        .await?;

    let resolved_overdue = resolve_overdue_rows.len() as u64;
    stats.resolved_overdue = resolved_overdue;

    if resolved_overdue > 0 {
        info!(
            count = resolved_overdue,
            kind = "overdue_schedule",
            "backup alert watcher: resolved overdue-schedule alerts (condition cleared)"
        );
    }

    // ── Step D: Resolve stalled alerts whose backup left 'pending' ────────────
    //
    // Clears open stalled_job alerts whose backup's state is no longer
    // 'pending' (it was dispatched, completed, failed, or cancelled).
    // Match on the message text since the FK column was dropped — slow
    // but correct, and the alert table is tiny.
    let sql_resolve_stalled = r#"
UPDATE backup_alerts a
SET resolved_at = NOW()
FROM backups b
WHERE a.resolved_at IS NULL
  AND a.kind = 'stalled_job'
  AND a.message LIKE 'Backup ' || b.id || ' (%'
  AND b.state != 'pending'
RETURNING a.id
"#;

    let resolve_stalled_rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql_resolve_stalled,
            vec![],
        ))
        .await?;

    let resolved_stalled = resolve_stalled_rows.len() as u64;
    stats.resolved_stalled = resolved_stalled;

    if resolved_stalled > 0 {
        info!(
            count = resolved_stalled,
            kind = "stalled_job",
            "backup alert watcher: resolved stalled-job alerts (job was claimed)"
        );
    }

    debug!(
        opened_overdue = stats.opened_overdue,
        opened_stalled = stats.opened_stalled,
        resolved_overdue = stats.resolved_overdue,
        resolved_stalled = stats.resolved_stalled,
        "backup alert sweep tick complete"
    );

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value as SVal};
    use std::collections::BTreeMap;

    /// Verify the function returns `Ok` with zero counts when there are no
    /// overdue schedules or stalled jobs. The MockDatabase is set up to return
    /// empty result sets for all four query_all calls (Steps A–D) so any
    /// unexpected SQL execution would panic with "exhausted mock results".
    ///
    /// `query_all` in Sea-ORM's MockDatabase consumes from the same
    /// `query_results` queue as `find_by_statement`. Providing four empty
    /// `BTreeMap` vecs simulates four INSERT/UPDATE ... RETURNING that touch
    /// zero rows.
    #[tokio::test]
    async fn sweep_returns_ok_on_empty_results() {
        let empty: Vec<BTreeMap<String, SVal>> = vec![];
        // Four query_all calls (Steps A, B, C, D) each return empty rows.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([empty.clone(), empty.clone(), empty.clone(), empty.clone()])
            .into_connection();

        let result = sweep_backup_alerts(&db).await;
        assert!(result.is_ok(), "sweep failed on empty DB: {:?}", result);

        let stats = result.unwrap();
        assert_eq!(stats.opened_overdue, 0);
        assert_eq!(stats.opened_stalled, 0);
        assert_eq!(stats.resolved_overdue, 0);
        assert_eq!(stats.resolved_stalled, 0);
        assert!(!stats.has_changes());
    }

    #[test]
    fn sweep_stats_has_changes_is_false_for_zero_stats() {
        let stats = SweepStats::default();
        assert!(!stats.has_changes());
    }

    #[test]
    fn sweep_stats_has_changes_is_true_when_any_field_nonzero() {
        let stats = SweepStats {
            opened_overdue: 1,
            ..SweepStats::default()
        };
        assert!(stats.has_changes());

        let stats = SweepStats {
            opened_stalled: 3,
            ..SweepStats::default()
        };
        assert!(stats.has_changes());

        let stats = SweepStats {
            resolved_overdue: 2,
            ..SweepStats::default()
        };
        assert!(stats.has_changes());

        let stats = SweepStats {
            resolved_stalled: 5,
            ..SweepStats::default()
        };
        assert!(stats.has_changes());
    }

    #[test]
    fn overdue_grace_is_one_hour() {
        assert_eq!(OVERDUE_GRACE, Duration::from_secs(3600));
    }
}
